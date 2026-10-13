//! Test composition for actor ordering, not a reducer, ledger, SQLite, PTY or
//! native deployment acceptance substitute. No adapter here is linked by main.
#![allow(dead_code)]

use ring::signature::{Ed25519KeyPair, KeyPair};
use sha2::{Digest, Sha256};
use spaceterm_core::{
    authority::*, control::*, identity::*, native_management::*, observations::*,
    operation_ledger_contracts::*, operations::*, organization::*, pairing::*, retirement::*,
    runtime_authority::*, storage::*,
};
use spaceterm_protocol::{
    ProtocolError,
    authentication::{self, SigningKey},
};
use spaceterm_server::{
    adapters::{core::*, terminal::*},
    api::*,
    contracts::*,
    extensions::*,
    failure::failure,
    shutdown::*,
};
use spaceterm_terminal::{
    appearance::{Appearance, AppearanceGeneration},
    runtime_contracts::*,
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

pub fn server() -> ServerId {
    ServerId::new([1; 16])
}
pub fn client(n: u64) -> ClientId {
    ClientId::new(server(), n).unwrap()
}
pub fn pane() -> PaneId {
    PaneId::new(server(), 1).unwrap()
}
pub fn workspace() -> WorkspaceId {
    WorkspaceId::new(server(), 1).unwrap()
}
pub fn tab() -> TabId {
    TabId::new(server(), 1).unwrap()
}
pub fn stream(n: u64) -> OperationStreamId {
    OperationStreamId::new(
        OperationStreamKey::new(server(), n).unwrap(),
        PairingId::new(server(), 1).unwrap(),
        client(n),
    )
    .unwrap()
}
pub fn operation(client: u64, sequence: u64) -> OperationId {
    OperationId::new(stream(client), OperationSequence::new(sequence).unwrap())
}
pub fn local(n: u64) -> [u8; 16] {
    let mut b = [0; 16];
    b[8..].copy_from_slice(&n.to_be_bytes());
    b
}
pub fn appearance() -> RuntimeAppearance {
    RuntimeAppearance {
        generation: AppearanceGeneration::INITIAL,
        foreground: spaceterm_core::content::Rgb {
            r: 255,
            g: 255,
            b: 255,
        },
        background: spaceterm_core::content::Rgb { r: 0, g: 0, b: 0 },
        palette: [spaceterm_core::content::Rgb { r: 0, g: 0, b: 0 }; 256],
        bold_as_bright: false,
        appearance: Appearance::Dark,
    }
}

pub struct Gate {
    entered: tokio::sync::Notify,
    release: tokio::sync::Semaphore,
}
impl Gate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Semaphore::new(0),
        })
    }
    pub async fn entered(&self) {
        self.entered.notified().await;
    }
    pub fn release(&self) {
        self.release.add_permits(1);
    }
    pub async fn wait(&self) {
        self.entered.notify_one();
        self.release.acquire().await.unwrap().forget();
    }
}
pub struct StoreGate {
    entered: tokio::sync::Notify,
    release: Mutex<bool>,
    condition: Condvar,
}
impl StoreGate {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: tokio::sync::Notify::new(),
            release: Mutex::new(false),
            condition: Condvar::new(),
        })
    }
    pub async fn entered(&self) {
        self.entered.notified().await;
    }
    pub fn release(&self) {
        *self.release.lock().unwrap() = true;
        self.condition.notify_one();
    }
    pub fn block(&self) {
        self.entered.notify_one();
        let mut release = self.release.lock().unwrap();
        while !*release {
            release = self.condition.wait(release).unwrap();
        }
    }
}

pub struct TestKey(Ed25519KeyPair);
impl TestKey {
    pub fn new(seed: u8) -> Arc<Self> {
        Arc::new(Self(
            Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap(),
        ))
    }
}
impl SigningKey for TestKey {
    fn public_key(&self) -> [u8; 32] {
        self.0.public_key().as_ref().try_into().unwrap()
    }
    fn sign(&self, bytes: &[u8]) -> Result<[u8; 64], ProtocolError> {
        Ok(self.0.sign(bytes).as_ref().try_into().unwrap())
    }
}
impl ServerCredentialPort for TestKey {}
pub struct Pairings {
    pub key: Arc<TestKey>,
    pub active: AtomicBool,
    pub clients: Mutex<BTreeMap<[u8; 16], bool>>,
}
impl authentication::PairingLookup for Pairings {
    fn lookup(
        &self,
        s: [u8; 16],
        p: [u8; 16],
        c: [u8; 16],
    ) -> Result<authentication::PairingRecord, ProtocolError> {
        if s != server().bytes() || p != local(1) || !self.clients.lock().unwrap().contains_key(&c)
        {
            return Err(ProtocolError::PairingDenied);
        }
        Ok(authentication::PairingRecord {
            principal: authentication::Principal {
                server: s,
                device: [2; 16],
                pairing: p,
                client: c,
            },
            device_public_key: self.key.public_key(),
            active: self.active.load(Ordering::SeqCst) && self.clients.lock().unwrap()[&c],
        })
    }
}
impl PairingAuthorizationPort for Pairings {}
pub fn principal(
    pairings: &Pairings,
    key: Arc<TestKey>,
    client: u64,
    generation: u64,
    epoch: u64,
) -> VerifiedPrincipal {
    let p = authentication::Principal {
        server: server().bytes(),
        device: [2; 16],
        pairing: local(1),
        client: local(client),
    };
    let pin = authentication::ServerPin {
        identity: p.server,
        public_key: key.public_key(),
    };
    let (request, hello) = authentication::ClientHandshake::begin(
        pin,
        p,
        authentication::Purpose::Ordinary,
        authentication::HandshakeConfig::default(),
        pairings.key.clone(),
    )
    .unwrap();
    let (server_handshake, reply) = authentication::ServerHandshake::accept(
        p.server,
        epoch,
        authentication::HandshakeConfig::default(),
        key,
        pairings,
        &hello,
    )
    .unwrap();
    let (request, proof) = request.receive_server(&reply).unwrap();
    let (established, ready) = server_handshake.receive_device(pairings, &proof).unwrap();
    let _ = request.receive_ready(&ready).unwrap();
    let authentication::Established::Ordinary(codec) = established else {
        panic!("ordinary handshake")
    };
    VerifiedPrincipal::from_authenticated(&codec, ConnectionGeneration::new(generation)).unwrap()
}

#[derive(Default)]
pub struct Codec {
    requests: Mutex<BTreeMap<Vec<u8>, CanonicalOperationRequest>>,
    outcomes: Mutex<Vec<Outcome>>,
}
impl LedgerCodec for Codec {
    fn canonical_request_digest(
        &self,
        _: &StreamBinding,
        id: OperationId,
        command: &Command,
    ) -> Result<RequestDigest, LedgerFailure> {
        let mut hash = Sha256::new();
        hash.update(b"test-only actor codec\0");
        hash.update(id.stream().key().local().to_be_bytes());
        hash.update(id.sequence().get().to_be_bytes());
        hash.update(RequestDigest::for_command(command).unwrap().bytes());
        Ok(RequestDigest::from_bytes(hash.finalize().into()))
    }
    fn encode_canonical_request(
        &self,
        binding: &StreamBinding,
        id: OperationId,
        command: &Command,
    ) -> Result<Vec<u8>, LedgerFailure> {
        let bytes = self
            .canonical_request_digest(binding, id, command)?
            .bytes()
            .to_vec();
        self.requests.lock().unwrap().insert(
            bytes.clone(),
            CanonicalOperationRequest::new(id, command.clone(), self)?,
        );
        Ok(bytes)
    }
    fn decode_canonical_request(
        &self,
        bytes: &[u8],
    ) -> Result<CanonicalOperationRequest, LedgerFailure> {
        self.requests
            .lock()
            .unwrap()
            .get(bytes)
            .cloned()
            .ok_or_else(|| LedgerFailure::new(LedgerFailureClass::InvalidRequest))
    }
    fn encode_compact_outcome(&self, outcome: &Outcome) -> Result<Vec<u8>, LedgerFailure> {
        let mut outcomes = self.outcomes.lock().unwrap();
        outcomes.push(outcome.clone());
        Ok((outcomes.len() as u64).to_be_bytes().to_vec())
    }
    fn decode_compact_outcome(&self, bytes: &[u8]) -> Result<Outcome, LedgerFailure> {
        let n = u64::from_be_bytes(
            bytes
                .try_into()
                .map_err(|_| LedgerFailure::new(LedgerFailureClass::InvalidCompactOutcome))?,
        ) as usize;
        self.outcomes
            .lock()
            .unwrap()
            .get(n.wrapping_sub(1))
            .cloned()
            .ok_or_else(|| LedgerFailure::new(LedgerFailureClass::InvalidCompactOutcome))
    }
}

pub struct Validator {
    pub rejected: AtomicBool,
}
impl DurabilityValidator for Validator {
    fn validate_transaction_projection(&self, _: &TransactionParts) -> Result<(), StoreFailure> {
        if self.rejected.load(Ordering::SeqCst) {
            Err(StoreFailure::InvalidTransaction)
        } else {
            Ok(())
        }
    }
    fn validate_recovery(&self, _: &RecoveredState) -> Result<(), StoreFailure> {
        if self.rejected.load(Ordering::SeqCst) {
            Err(StoreFailure::InvalidDurableState)
        } else {
            Ok(())
        }
    }
}

pub struct Database {
    pub recovered: RecoveredState,
    pub transactions: Vec<TransactionParts>,
    pub next: Option<CommitDisposition>,
    pub gate: Option<Arc<StoreGate>>,
    pub recovery_failure: Option<StoreFailure>,
    pub log: Vec<&'static str>,
    pub close_observations: Vec<(CloseWorkObservation, MonotonicTick)>,
    pub reject_stream_registration: bool,
}
pub struct Store(pub Arc<Mutex<Database>>);
impl OrganizationStore for Store {
    fn recover(&mut self) -> Result<RecoveredState, StoreFailure> {
        let mut db = self.0.lock().unwrap();
        db.log.push("recover");
        if let Some(error) = db.recovery_failure {
            return Err(error);
        }
        Ok(db.recovered.clone())
    }
    fn commit(&mut self, transaction: &DurableTransaction) -> CommitDisposition {
        let gate = {
            let mut db = self.0.lock().unwrap();
            db.log.push("commit entered");
            db.transactions.push(transaction.parts().clone());
            db.gate.take()
        };
        if let Some(gate) = gate {
            gate.block();
        }
        let mut db = self.0.lock().unwrap();
        if let Some(disposition) = db.next.take() {
            return disposition;
        }
        let p = transaction.parts();
        if p.expected_store_revision != db.recovered.store_revision {
            return CommitDisposition::NotApplied(StoreFailure::RevisionConflict);
        }
        db.recovered.store_revision = p.resulting_store_revision;
        db.recovered.last_epoch = p.resulting_epoch;
        db.recovered.high_water = p.resulting_high_water;
        db.recovered.organization.revision = p.resulting_organization_revision;
        for write in &p.organization_writes {
            match write {
                OrganizationWrite::PutWorkspace(v) => {
                    db.recovered
                        .organization
                        .workspaces
                        .retain(|old| old.id != v.id);
                    db.recovered.organization.workspaces.push(v.clone());
                }
                OrganizationWrite::DeleteWorkspace(id) => db
                    .recovered
                    .organization
                    .workspaces
                    .retain(|old| old.id != *id),
                OrganizationWrite::PutPane(v) => {
                    db.recovered.organization.panes.retain(|old| old.id != v.id);
                    db.recovered.organization.panes.push(v.clone());
                }
                OrganizationWrite::DeletePane(id) => {
                    db.recovered.organization.panes.retain(|old| old.id != *id)
                }
                OrganizationWrite::DeleteTab(id) => {
                    db.recovered.organization.tabs.retain(|old| old.id != *id)
                }
                OrganizationWrite::Retained(spaceterm_core::organization_retained::RetainedOrganizationWrite::SetPaneGenerationHighWater(value)) => {
                    let mut retained = db.recovered.organization.retained.as_ref().unwrap().parts().clone();
                    retained.pane_generations.retain(|old| old.pane() != value.pane());
                    retained.pane_generations.push(*value);
                    db.recovered.organization.retained = Some(spaceterm_core::organization_retained::OrganizationRetainedFacts::new(retained).unwrap());
                }
                OrganizationWrite::Retained(spaceterm_core::organization_retained::RetainedOrganizationWrite::SetRemoteAccess(value)) => {
                    let mut retained = db.recovered.organization.retained.as_ref().unwrap().parts().clone();
                    retained.remote_access = value.clone();
                    db.recovered.organization.retained = Some(spaceterm_core::organization_retained::OrganizationRetainedFacts::new(retained).unwrap());
                }
                _ => {}
            }
        }
        for write in &p.operation_writes {
            if let OperationWrite::Pairing(write) = write {
                let mut pairing = db
                    .recovered
                    .pairing
                    .as_ref()
                    .map(|value| value.parts().clone())
                    .unwrap_or(PairingRecoveryParts {
                        authorizations: vec![],
                        enrollments: vec![],
                    });
                match write.as_ref() {
                    PairingWrite::PutAuthorization { value, .. } => {
                        pairing
                            .authorizations
                            .retain(|old| old.parts().record.id != value.parts().record.id);
                        pairing.authorizations.push(value.clone());
                    }
                    PairingWrite::PutEnrollmentReservation { value, .. } => {
                        pairing
                            .enrollments
                            .retain(|old| old.parts().ticket != value.parts().ticket);
                        pairing.enrollments.push(value.clone());
                    }
                }
                db.recovered.pairing = Some(PairingRecovery::new(pairing).unwrap());
            }
            if let OperationWrite::NativeRetirement(value) = write {
                match value {
                    NativeRetirementMutation::Accept(value) => {
                        db.recovered.native_retirements.push(*value.clone())
                    }
                    NativeRetirementMutation::Progress { expected, next } => {
                        let old = db
                            .recovered
                            .native_retirements
                            .iter_mut()
                            .find(|value| value.request() == expected.request())
                            .unwrap();
                        assert_eq!(old, expected.as_ref());
                        *old = next.as_ref().clone();
                    }
                    NativeRetirementMutation::Complete { request, .. } => db
                        .recovered
                        .native_retirements
                        .retain(|v| v.request() != *request),
                }
            }
        }
        db.log.push("durable");
        CommitDisposition::Durable(CommitReceipt {
            store_revision: p.resulting_store_revision,
            organization_revision: p.resulting_organization_revision,
        })
    }
}

pub struct Reducer {
    snapshot: Arc<OrganizationSnapshot>,
    staged: Option<Arc<OrganizationSnapshot>>,
    high_water: IdentityHighWater,
    pub codec: Arc<Codec>,
    pub log: Arc<Mutex<Database>>,
}
impl Reducer {
    fn projection(
        &mut self,
        mut state: OrganizationState,
        runtime: Vec<PaneRuntimeRecord>,
        epoch: ServerEpoch,
        mut writes: Vec<OrganizationWrite>,
        auxiliary: Vec<OperationWrite>,
        outcome: Outcome,
    ) -> PreparedReduction {
        if let Some(retained) = &state.retained {
            use spaceterm_core::organization_retained::*;
            let mut parts = retained.parts().clone();
            for record in &runtime {
                let value = PaneGenerationHighWater::new(
                    record.pane,
                    record.session_generation,
                    record.control_generation,
                );
                if parts
                    .pane_generations
                    .iter()
                    .find(|old| old.pane() == record.pane)
                    != Some(&value)
                {
                    parts
                        .pane_generations
                        .retain(|old| old.pane() != record.pane);
                    parts.pane_generations.push(value);
                    writes.push(OrganizationWrite::Retained(
                        RetainedOrganizationWrite::SetPaneGenerationHighWater(value),
                    ));
                }
            }
            parts.pane_generations.sort_by_key(|value| value.pane());
            state.retained = Some(OrganizationRetainedFacts::new(parts).unwrap());
        }
        let snapshot = Arc::new(OrganizationSnapshot::new(epoch, state, runtime).unwrap());
        self.staged = Some(snapshot.clone());
        PreparedReduction {
            snapshot,
            delta: None,
            high_water: self.high_water,
            organization_writes: writes,
            auxiliary_writes: auxiliary,
            outcome,
            retirement: None,
            native_retirement: None,
        }
    }
}
impl ReducerPort for Reducer {
    fn recover(
        &mut self,
        recovered: &RecoveredState,
        epoch: ServerEpoch,
        _: &[SessionCleanupObservation],
    ) -> Result<PreparedReduction, ServerFailure> {
        self.high_water = recovered.high_water;
        let runtime = recovered
            .organization
            .panes
            .iter()
            .map(|p| PaneRuntimeRecord {
                pane: p.id,
                session_generation: SessionGeneration::new(0),
                phase: SessionPhase::Ended {
                    reason: EndReason::ServerRestart,
                },
                control_generation: ControlGeneration::new(0),
                control: ControlRecord::Unclaimed,
            })
            .collect();
        Ok(self.projection(
            recovered.organization.clone(),
            runtime,
            epoch,
            vec![],
            vec![],
            Outcome::Applied,
        ))
    }
    fn snapshot(&self) -> Arc<OrganizationSnapshot> {
        self.snapshot.clone()
    }
    fn observe_close_work(
        &mut self,
        observation: CloseWorkObservation,
        now: MonotonicTick,
    ) -> Result<(), ServerFailure> {
        self.log
            .lock()
            .unwrap()
            .close_observations
            .push((observation, now));
        Ok(())
    }
    fn prepare(
        &mut self,
        request: ReductionRequest<'_>,
        _: CommitCursor,
    ) -> Result<PreparedReduction, ServerFailure> {
        let mut state = self.snapshot.state().clone();
        let mut runtime = self.snapshot.runtime().to_vec();
        let mut writes = vec![];
        let mut auxiliary = vec![];
        let mut outcome = Outcome::Applied;
        let mut retirement = None;
        let mut native_retirement = None;
        match request {
            ReductionRequest::Control { transition, .. } => {
                runtime
                    .iter_mut()
                    .find(|v| v.pane == transition.next().pane)
                    .unwrap()
                    .clone_from(&transition.next());
                state.revision = state.revision.checked_successor().unwrap();
                outcome = transition
                    .granted()
                    .map_or(Outcome::Applied, Outcome::ControlGranted);
            }
            ReductionRequest::Rejected { failure, .. } => outcome = Outcome::Failed(failure),
            ReductionRequest::Command(request)
            | ReductionRequest::ValidatedCommand { request, .. } => match request.command() {
                Command::RenameWorkspace {
                    draft: NameDraft::Custom(name),
                    ..
                } => {
                    state.workspaces[0].name.value = WorkspaceName {
                        mode: NameMode::Custom,
                        display: name.clone(),
                    };
                    state.workspaces[0].name.version = state.workspaces[0]
                        .name
                        .version
                        .checked_successor()
                        .unwrap();
                    writes.push(OrganizationWrite::PutWorkspace(state.workspaces[0].clone()));
                    state.revision = state.revision.checked_successor().unwrap();
                }
                Command::SetRemoteAccess {
                    expected_version,
                    value,
                    ..
                } => {
                    use spaceterm_core::organization_retained::*;
                    let mut retained = state.retained.as_ref().unwrap().parts().clone();
                    assert_eq!(retained.remote_access.version, *expected_version);
                    retained.remote_access = Versioned {
                        version: expected_version.checked_successor().unwrap(),
                        value: value.clone(),
                    };
                    writes.push(OrganizationWrite::Retained(
                        RetainedOrganizationWrite::SetRemoteAccess(retained.remote_access.clone()),
                    ));
                    state.retained = Some(OrganizationRetainedFacts::new(retained).unwrap());
                    state.revision = state.revision.checked_successor().unwrap();
                }
                Command::BeginStartup(intent) => {
                    outcome = Outcome::StartupAwaitingMeasurement {
                        intent: intent.id(),
                    }
                }
                Command::CloseConfirmed { assessment, .. }
                | Command::EndAllSessions {
                    assessment,
                    server: _,
                } => {
                    let target = match request.command() {
                        Command::CloseConfirmed { target, .. } => AssessmentTarget::Close(*target),
                        _ => AssessmentTarget::EndAllSessions(server()),
                    };
                    state.revision = state.revision.checked_successor().unwrap();
                    let old = runtime[0];
                    runtime[0].session_generation =
                        old.session_generation.checked_successor().unwrap();
                    runtime[0].phase = SessionPhase::Ended {
                        reason: EndReason::EndAllSessions,
                    };
                    if matches!(target, AssessmentTarget::Close(_)) {
                        writes = vec![
                            OrganizationWrite::DeletePane(pane()),
                            OrganizationWrite::DeleteTab(tab()),
                            OrganizationWrite::DeleteWorkspace(workspace()),
                        ];
                        state.panes.clear();
                        state.tabs.clear();
                        state.workspaces.clear();
                        runtime.clear();
                    }
                    let reason = if matches!(target, AssessmentTarget::Close(_)) {
                        RetirementReason::ConfirmedClose
                    } else {
                        RetirementReason::EndAllSessions
                    };
                    let sessions = if old.session_generation.get() > 0 {
                        vec![
                            RetiredSession::new(
                                SessionKey {
                                    epoch: self.snapshot.epoch(),
                                    pane: pane(),
                                    generation: old.session_generation,
                                },
                                reason,
                                RetirementCleanupState::AwaitingEvidence,
                            )
                            .unwrap(),
                        ]
                    } else {
                        vec![]
                    };
                    retirement = Some(
                        RetirementDescriptor::new(RetirementDescriptorParts {
                            batch: RetirementBatchId::new(
                                server(),
                                self.snapshot.epoch(),
                                request.binding().id.sequence().get(),
                            )
                            .unwrap(),
                            binding: request.binding(),
                            assessment: *assessment,
                            acceptance_revision: state.revision,
                            target,
                            panes: vec![
                                RetirementPane::new(
                                    workspace(),
                                    tab(),
                                    pane(),
                                    None,
                                    old.session_generation.checked_successor().unwrap(),
                                )
                                .unwrap(),
                            ],
                            sessions,
                            completion: RetirementCompletion::AppliedAtAcceptanceRevision,
                            eventual_result: CompactOutcome::new(
                                &Outcome::Applied,
                                self.codec.as_ref(),
                            )
                            .unwrap(),
                        })
                        .unwrap(),
                    );
                }
                _ => {}
            },
            ReductionRequest::StartupPreparationFailed {
                intent, failure, ..
            } => {
                let record = runtime
                    .iter_mut()
                    .find(|record| record.pane == intent.fence().session().pane)
                    .unwrap();
                if failure == OperationFailure::DirectoryUnavailable {
                    record.phase = SessionPhase::DirectoryUnavailable;
                    state.revision = state.revision.checked_successor().unwrap();
                }
                outcome = Outcome::Failed(failure);
            }
            ReductionRequest::StartupReserved { reservation, .. } => {
                let record = runtime
                    .iter_mut()
                    .find(|v| v.pane == reservation.session().pane)
                    .unwrap();
                record.session_generation = reservation.session().generation;
                record.phase = SessionPhase::Starting {
                    generation: record.session_generation,
                };
                state.revision = state.revision.checked_successor().unwrap();
                outcome = Outcome::SessionStarted(reservation.session());
            }
            ReductionRequest::RuntimeStarted(session) => {
                let record = runtime
                    .iter_mut()
                    .find(|v| v.pane == session.pane && v.session_generation == session.generation)
                    .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
                record.phase = SessionPhase::Running {
                    generation: session.generation,
                };
                state.revision = state.revision.checked_successor().unwrap();
            }
            ReductionRequest::RuntimeCleanup(_) => {}
            ReductionRequest::RuntimeFinished(terminal)
            | ReductionRequest::UnstartedLaunchFailed { terminal, .. } => {
                let record = runtime
                    .iter_mut()
                    .find(|v| {
                        v.pane == terminal.session().pane
                            && v.session_generation == terminal.session().generation
                    })
                    .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
                match terminal.status() {
                    RuntimeTerminalStatus::OrdinaryExit { .. } => {
                        writes = vec![
                            OrganizationWrite::DeletePane(pane()),
                            OrganizationWrite::DeleteTab(tab()),
                            OrganizationWrite::DeleteWorkspace(workspace()),
                        ];
                        state.panes.clear();
                        state.tabs.clear();
                        state.workspaces.clear();
                        runtime.clear();
                    }
                    RuntimeTerminalStatus::StartupFailed { class }
                    | RuntimeTerminalStatus::RuntimeFailed { class } => {
                        record.phase = SessionPhase::Failed { class }
                    }
                    _ => {}
                }
                state.revision = state.revision.checked_successor().unwrap();
            }
            ReductionRequest::RestorationDirectoryConfirmed(observation) => {
                let p = state
                    .panes
                    .iter_mut()
                    .find(|v| v.id == observation.session().pane)
                    .unwrap();
                p.confirmed_restoration_directory.value = Some(observation.directory().clone());
                p.confirmed_restoration_directory.version = p
                    .confirmed_restoration_directory
                    .version
                    .checked_successor()
                    .unwrap();
                writes.push(OrganizationWrite::PutPane(p.clone()));
                state.revision = state.revision.checked_successor().unwrap();
            }
            ReductionRequest::RetirementCompleted { .. } => {}
            ReductionRequest::NativeRetirementAccepted {
                request,
                action,
                sessions,
            } => {
                state.revision = state.revision.checked_successor().unwrap();
                for record in &mut runtime {
                    record.session_generation =
                        record.session_generation.checked_successor().unwrap();
                    record.phase = SessionPhase::Ended {
                        reason: EndReason::Stop,
                    };
                }
                let mut panes: Vec<_> = runtime
                    .iter()
                    .map(|v| {
                        RetirementPane::new(workspace(), tab(), v.pane, None, v.session_generation)
                            .unwrap()
                    })
                    .collect();
                for retained in sessions {
                    if !panes
                        .iter()
                        .any(|pane| pane.pane() == retained.session().pane)
                    {
                        panes.push(
                            RetirementPane::new(
                                workspace(),
                                tab(),
                                retained.session().pane,
                                None,
                                retained.session().generation.checked_successor().unwrap(),
                            )
                            .unwrap(),
                        );
                    }
                }
                let sessions = sessions.to_vec();
                let descriptor = NativeRetirementDescriptor::new(NativeRetirementDescriptorParts {
                    request,
                    batch: RetirementBatchId::new(server(), self.snapshot.epoch(), 99).unwrap(),
                    action,
                    acceptance_revision: state.revision,
                    panes,
                    sessions,
                })
                .unwrap();
                auxiliary.push(OperationWrite::NativeRetirement(
                    NativeRetirementMutation::Accept(Box::new(descriptor.clone())),
                ));
                native_retirement = Some(descriptor);
            }
            ReductionRequest::NativeRetirementObserved {
                descriptor,
                cleanup,
            } => {
                let mut parts = descriptor.parts().clone();
                for session in &mut parts.sessions {
                    let result = cleanup
                        .iter()
                        .find(|value| value.session() == session.session())
                        .unwrap()
                        .result();
                    let state = match result {
                        CleanupResult::Confirmed(value) => RetirementCleanupState::Confirmed(value),
                        CleanupResult::Failed(value) => RetirementCleanupState::Failed(value),
                    };
                    *session =
                        RetiredSession::new(session.session(), session.first_reason(), state)
                            .unwrap();
                }
                auxiliary.push(OperationWrite::NativeRetirement(
                    NativeRetirementMutation::Progress {
                        expected: Box::new(descriptor.clone()),
                        next: Box::new(NativeRetirementDescriptor::new(parts).unwrap()),
                    },
                ));
            }
            ReductionRequest::NativeRetirementCompleted { descriptor, .. } => auxiliary.push(
                OperationWrite::NativeRetirement(NativeRetirementMutation::Complete {
                    request: descriptor.request(),
                    batch: descriptor.batch(),
                }),
            ),
            ReductionRequest::ReleaseClients(clients) => {
                for record in &mut runtime {
                    if matches!(record.control,ControlRecord::Held{client,..} if clients.contains(&client))
                    {
                        record.control = ControlRecord::Unclaimed;
                        record.control_generation =
                            record.control_generation.checked_successor().unwrap();
                    }
                }
                state.revision = state.revision.checked_successor().unwrap();
            }
            ReductionRequest::PairingCompletion {
                allocation,
                revoked_clients,
                ..
            } => {
                if let PairingAllocationRequest::AdditionalClient(_) = allocation {
                    self.high_water.client = self.high_water.client.checked_add(1).unwrap();
                    self.high_water.operation_stream =
                        self.high_water.operation_stream.checked_add(2).unwrap();
                }
                let mut changed = false;
                for record in &mut runtime {
                    if matches!(record.control, ControlRecord::Held { client, .. } if revoked_clients.contains(&client))
                    {
                        record.control = ControlRecord::Unclaimed;
                        record.control_generation =
                            record.control_generation.checked_successor().unwrap();
                        changed = true;
                    }
                }
                if changed {
                    state.revision = state.revision.checked_successor().unwrap();
                }
            }
            ReductionRequest::StartedGitRetirement { descriptor, .. } => {
                use spaceterm_core::organization_retained::*;
                let mut retained = state.retained.as_ref().unwrap().parts().clone();
                for record in &mut runtime {
                    if descriptor
                        .panes()
                        .iter()
                        .any(|pane| pane.pane() == record.pane)
                    {
                        record.session_generation =
                            record.session_generation.checked_successor().unwrap();
                        record.phase = SessionPhase::Ended {
                            reason: EndReason::WorktreeRemoval,
                        };
                        let high_water = PaneGenerationHighWater::new(
                            record.pane,
                            record.session_generation,
                            record.control_generation,
                        );
                        retained
                            .pane_generations
                            .retain(|value| value.pane() != record.pane);
                        retained.pane_generations.push(high_water);
                        writes.push(OrganizationWrite::Retained(
                            RetainedOrganizationWrite::SetPaneGenerationHighWater(high_water),
                        ));
                    }
                }
                state.retained = Some(OrganizationRetainedFacts::new(retained).unwrap());
                state.revision = state.revision.checked_successor().unwrap();
                retirement = Some(descriptor.clone());
            }
            ReductionRequest::ReserveGitAllocation {
                binding,
                repository,
                worktree,
                fallbacks,
            } => {
                use spaceterm_core::worktrees::intent::*;
                assert!(
                    fallbacks.is_empty(),
                    "this fixture tests only worktree allocation choreography"
                );
                let old = self.high_water;
                let mut next = old;
                let repository = repository.then(|| {
                    next.repository += 1;
                    RepositoryId::new(server(), next.repository).unwrap()
                });
                let worktree = worktree.then(|| {
                    next.worktree += 1;
                    WorktreeId::new(server(), next.worktree).unwrap()
                });
                let reservation = GitAllocationReservation::new(GitAllocationReservationParts {
                    binding,
                    expected_high_water: old,
                    resulting_high_water: next,
                    repository,
                    worktree,
                    fallbacks: vec![],
                })
                .unwrap();
                self.high_water = next;
                auxiliary.push(OperationWrite::GitAllocation(
                    GitAllocationMutation::Reserve(Box::new(reservation)),
                ));
            }
            ReductionRequest::SettleStartedGit {
                intent,
                revoked_clients,
                ..
            } => {
                self.log.lock().unwrap().log.push("git settlement");
                for record in &mut runtime {
                    if matches!(record.control, ControlRecord::Held { client, .. } if revoked_clients.contains(&client))
                    {
                        record.control = ControlRecord::Unclaimed;
                        record.control_generation =
                            record.control_generation.checked_successor().unwrap();
                    }
                }
                state.revision = state.revision.checked_successor().unwrap();
                auxiliary.push(OperationWrite::GitIntent(
                    spaceterm_core::worktrees::intent::GitIntentMutation::Complete {
                        binding: intent.binding(),
                    },
                ));
            }
        }
        let mut reduction = self.projection(
            state,
            runtime,
            self.snapshot.epoch(),
            writes,
            auxiliary,
            outcome,
        );
        reduction.retirement = retirement;
        reduction.native_retirement = native_retirement;
        Ok(reduction)
    }
    fn committed(&mut self, _: CommitReceipt) -> Result<(), ServerFailure> {
        self.log.lock().unwrap().log.push("reducer commit");
        if let Some(snapshot) = self.staged.take() {
            self.snapshot = snapshot;
        }
        Ok(())
    }
    fn not_applied(&mut self) {
        self.staged.take();
        self.high_water = self.log.lock().unwrap().recovered.high_water;
    }
}

#[derive(Clone)]
pub struct Entry {
    pub binding: OperationBinding,
    pub kind: PendingKind,
    pub outcome: Option<(Outcome, OrganizationRevision)>,
    pub admitted: StoreRevision,
    pub progress: Option<PendingExternalProgress>,
}
pub struct Ledger {
    pub entries: BTreeMap<OperationId, Entry>,
    streams: BTreeMap<OperationStreamId, StreamRecord>,
    staged: Option<BTreeMap<OperationId, Entry>>,
    staged_streams: Option<BTreeMap<OperationStreamId, StreamRecord>>,
    codec: Arc<Codec>,
    log: Arc<Mutex<Database>>,
}
impl Ledger {
    fn stage(
        &mut self,
        context: TransactionContext,
        write: LedgerWrite,
        entries: BTreeMap<OperationId, Entry>,
    ) -> Result<LedgerProjection, ServerFailure> {
        self.staged = Some(entries);
        LedgerProjection::new(context, vec![write])
            .map_err(spaceterm_server::failure::ledger_failure)
    }
}
impl LedgerPort for Ledger {
    fn recover(&mut self, recovered: &RecoveredState) -> Result<(), ServerFailure> {
        if let Some(ledger) = &recovered.ledger {
            self.streams = ledger
                .streams
                .iter()
                .map(|header| (header.binding.stream(), header.clone()))
                .collect();
            for record in ledger.operations() {
                if let RecoveredOperationRecord::Pending(v) = record {
                    self.entries.insert(
                        v.binding.id,
                        Entry {
                            binding: v.binding,
                            kind: v.kind,
                            outcome: None,
                            admitted: v.admitted_at,
                            progress: v.external_progress.clone(),
                        },
                    );
                }
            }
        }
        Ok(())
    }
    fn prepare_recovery(
        &mut self,
        _: &RecoveredState,
        _: &[SessionCleanupObservation],
        _: TransactionContext,
    ) -> Result<Option<LedgerProjection>, ServerFailure> {
        Ok(None)
    }
    fn lookup(&self, binding: OperationBinding) -> Result<OperationLookup, ServerFailure> {
        if !self.streams.contains_key(&binding.id.stream()) {
            return Err(failure(ServerFailureClass::ScopeMismatch));
        }
        match self.entries.get(&binding.id) {
            Some(value) if value.binding != binding => {
                Ok(OperationLookup::IdentityReusedWithDifferentContents)
            }
            Some(value) => {
                Ok(value
                    .outcome
                    .as_ref()
                    .map_or(OperationLookup::Pending, |(result, revision)| {
                        OperationLookup::Completed {
                            result: result.clone(),
                            publication_revision: *revision,
                        }
                    }))
            }
            None => Ok(OperationLookup::NeverAdmitted),
        }
    }
    fn resume_atomic(
        &mut self,
        request: &CanonicalOperationRequest,
    ) -> Result<bool, ServerFailure> {
        Ok(self.entries.get(&request.binding().id).is_some_and(|v| {
            v.binding == request.binding()
                && v.kind == PendingKind::AtomicMutation
                && v.outcome.is_none()
        }))
    }
    fn prepare_admission(
        &mut self,
        request: &CanonicalOperationRequest,
        kind: PendingKind,
        context: TransactionContext,
    ) -> Result<LedgerProjection, ServerFailure> {
        let mut entries = self.entries.clone();
        let admitted = context.expected_store().checked_successor().unwrap();
        let progress = match kind {
            PendingKind::AtomicMutation => None,
            PendingKind::External { effect } => Some(
                PendingExternalProgress::new(PendingExternalProgressParts {
                    binding: request.binding(),
                    effect,
                    state: ExternalProgressState::AwaitingIntent,
                })
                .unwrap(),
            ),
        };
        entries.insert(
            request.binding().id,
            Entry {
                binding: request.binding(),
                kind,
                outcome: None,
                admitted,
                progress: progress.clone(),
            },
        );
        self.log.lock().unwrap().log.push("ledger admission");
        self.stage(
            context,
            LedgerWrite::AdmitOperation(PendingOperationRecord {
                binding: request.binding(),
                kind,
                reservation: OutcomeReservation::full(ReservationClass::Ordinary),
                admitted_at: admitted,
                admission_organization_revision: context.base_organization(),
                progress_recorded_at: admitted,
                external_progress: progress,
            }),
            entries,
        )
    }
    fn prepare_external_progress(
        &mut self,
        binding: OperationBinding,
        progress: PendingExternalProgress,
        context: TransactionContext,
    ) -> Result<LedgerProjection, ServerFailure> {
        let mut entries = self.entries.clone();
        let entry = entries
            .get_mut(&binding.id)
            .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
        entry.progress = Some(progress.clone());
        self.stage(
            context,
            LedgerWrite::RecordExternalProgress {
                previous_progress_store_revision: entry.admitted,
                record: PendingOperationRecord {
                    binding,
                    kind: entry.kind,
                    reservation: OutcomeReservation::full(ReservationClass::Ordinary),
                    admitted_at: entry.admitted,
                    admission_organization_revision: context.base_organization(),
                    progress_recorded_at: context.expected_store().checked_successor().unwrap(),
                    external_progress: Some(progress),
                },
            },
            entries,
        )
    }
    fn prepare_completion(
        &mut self,
        binding: OperationBinding,
        outcome: &Outcome,
        revision: OrganizationRevision,
        context: TransactionContext,
    ) -> Result<LedgerProjection, ServerFailure> {
        let mut entries = self.entries.clone();
        let entry = entries
            .get_mut(&binding.id)
            .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
        let admitted = entry.admitted;
        entry.outcome = Some((outcome.clone(), revision));
        let record = CompletedOperationRecord {
            binding,
            reservation: OutcomeReservation::full(ReservationClass::Ordinary),
            admitted_at: admitted,
            completed_at: context.expected_store().checked_successor().unwrap(),
            outcome: CompactOutcome::new(outcome, self.codec.as_ref()).unwrap(),
            outcome_revision: revision,
        };
        self.stage(
            context,
            LedgerWrite::CompleteOperation {
                record,
                basis: context.operation_basis().map_or(
                    CompletionRevisionBasis::RecordedProgress,
                    CompletionRevisionBasis::OwnedReduction,
                ),
            },
            entries,
        )
    }
    fn prepare_pairing_completion(
        &mut self,
        binding: OperationBinding,
        streams: &[StreamRecord],
        retired_streams: &[StreamBinding],
        outcome: &Outcome,
        revision: OrganizationRevision,
        context: TransactionContext,
    ) -> Result<LedgerProjection, ServerFailure> {
        if !streams.is_empty() && self.log.lock().unwrap().reject_stream_registration {
            return Err(failure(ServerFailureClass::Capacity));
        }
        let mut next = self.streams.clone();
        let mut writes = vec![];
        for stream in streams {
            if next
                .insert(stream.binding.stream(), stream.clone())
                .is_some()
            {
                return Err(failure(ServerFailureClass::ScopeMismatch));
            }
            writes.push(LedgerWrite::RegisterStream(stream.clone()));
        }
        for binding in retired_streams {
            next.get_mut(&binding.stream()).unwrap().status =
                StreamStatus::Retired(StreamRetirementReason::PairingRevoked);
            writes.push(LedgerWrite::RetireStream {
                binding: *binding,
                reason: StreamRetirementReason::PairingRevoked,
            });
        }
        let completion = self.prepare_completion(binding, outcome, revision, context)?;
        writes.extend_from_slice(completion.writes());
        self.staged_streams = Some(next);
        LedgerProjection::new(context, writes).map_err(spaceterm_server::failure::ledger_failure)
    }
    fn prepare_acknowledgement(
        &mut self,
        binding: StreamBinding,
        through: OperationSequence,
        context: TransactionContext,
    ) -> Result<LedgerProjection, ServerFailure> {
        self.stage(
            context,
            LedgerWrite::AcknowledgePrefix {
                binding,
                previous: 0,
                through: through.get(),
            },
            self.entries.clone(),
        )
    }
    fn committed(&mut self, _: CommitReceipt) -> Result<(), ServerFailure> {
        self.log.lock().unwrap().log.push("ledger commit");
        if let Some(entries) = self.staged.take() {
            self.entries = entries;
        }
        if let Some(streams) = self.staged_streams.take() {
            self.streams = streams;
        }
        Ok(())
    }
    fn not_applied(&mut self) {
        self.staged.take();
        self.staged_streams.take();
    }
}

pub struct Host {
    pub cleanup: Mutex<Vec<SessionCleanupObservation>>,
    pub requests: Mutex<Vec<Vec<SessionKey>>>,
    pub reject_start: AtomicBool,
}
impl StartupAuthorityPort for Host {
    fn validate_startup(&self) -> Result<ServerId, ServerFailure> {
        if self.reject_start.load(Ordering::SeqCst) {
            Err(failure(ServerFailureClass::NativeUnavailable))
        } else {
            Ok(server())
        }
    }
    fn validate_credentials(
        &self,
        _: &dyn ServerCredentialPort,
        s: ServerId,
    ) -> Result<(), ServerFailure> {
        assert_eq!(s, server());
        Ok(())
    }
}
impl HostManagementPort for Host {
    fn previous_instance_cleanup<'a>(
        &'a self,
        _: ServerId,
        _: ServerEpoch,
        sessions: &'a [SessionKey],
    ) -> ServerFuture<'a, Result<Vec<SessionCleanupObservation>, ServerFailure>> {
        self.requests.lock().unwrap().push(sessions.to_vec());
        let result = self.cleanup.lock().unwrap().clone();
        Box::pin(async move { Ok(result) })
    }
}
impl StoppedAuthorizationPort for Host {
    fn verify_stopped(
        &self,
        s: ServerId,
        _: ServerEpoch,
    ) -> Result<(NativeManagementRequestRef, NativeRetirementAction), ServerFailure> {
        if self.reject_start.load(Ordering::SeqCst) {
            return Err(failure(ServerFailureClass::NativeUnavailable));
        }
        Ok((
            NativeManagementRequestRef::new(s, [3; 16], RequestDigest::from_bytes([4; 32]))
                .unwrap(),
            NativeRetirementAction::Stop,
        ))
    }
}
pub struct Clock;
impl ServerClock for Clock {
    fn now(&self) -> Result<MonotonicTick, ServerFailure> {
        Ok(MonotonicTick::new(1000))
    }
}
pub struct External {
    pub reads_cancelled: AtomicUsize,
    pub finished: AtomicUsize,
    pub gate: Mutex<Option<Arc<Gate>>>,
}
impl ExternalWorkPort for External {
    fn cancel_reads(&self) {
        self.reads_cancelled.fetch_add(1, Ordering::SeqCst);
    }
    fn finish_started_writes(&self) -> ServerFuture<'_, Result<(), ServerFailure>> {
        let gate = self.gate.lock().unwrap().clone();
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.wait().await;
            }
            self.finished.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
    fn reconcile<'a>(
        &'a self,
        _: &'a RecoveredState,
    ) -> ServerFuture<'a, Result<RecoveryEdits, ServerFailure>> {
        Box::pin(async {
            Ok(RecoveryEdits {
                transactions: vec![],
            })
        })
    }
}
pub struct Observer {
    pub admitted: AtomicUsize,
    pub directories: AtomicUsize,
    pub pairing_commits: AtomicUsize,
    pub pairings: Arc<Pairings>,
}
impl ExtensionObserverPort for Observer {
    fn admitted(
        &self,
        _: CanonicalOperationRequest,
        _: CommitReceipt,
        _: ServerHandle,
    ) -> Result<(), ServerFailure> {
        self.admitted.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn pairing_committed(
        &self,
        writes: &PairingWrites,
        _: CommitReceipt,
    ) -> Result<(), ServerFailure> {
        for write in &writes.parts().writes {
            if let PairingWrite::PutAuthorization { value, .. } = write {
                for client in &value.parts().clients {
                    self.pairings.clients.lock().unwrap().insert(
                        local(client.parts().client.id.local()),
                        value.parts().record.state == PairingState::Authorized,
                    );
                }
            }
        }
        self.pairing_commits.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn confirmed_directory_committed(&self, _: &ConfirmedDirectoryObservation, _: CommitReceipt) {
        self.directories.fetch_add(1, Ordering::SeqCst);
    }
}

pub struct RuntimeState {
    pub authority: RuntimeAuthority,
    pub lifecycle: RuntimeLifecycle,
    pub directory: Option<ConfirmedDirectoryObservation>,
    pub close_work: Option<CloseWorkObservation>,
    pub cleanup: Option<SessionCleanupObservation>,
    pub retire_calls: usize,
}
pub struct Runtime {
    pub released: Arc<AtomicUsize>,
    pub release_notice: Arc<tokio::sync::Notify>,
    pub state: Arc<Mutex<RuntimeState>>,
    pub retirement_gate: Arc<Mutex<Option<Arc<Gate>>>>,
    pub barrier_gate: Arc<Mutex<Option<Arc<Gate>>>>,
    pub cleanup_failed: Arc<AtomicBool>,
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.released.fetch_add(1, Ordering::SeqCst);
        self.release_notice.notify_one();
    }
}
impl RuntimeObservationSource for Runtime {
    fn session(&self) -> SessionKey {
        self.state.lock().unwrap().authority.session()
    }
    fn display_directory_snapshot(&self) -> Option<DisplayDirectoryObservation> {
        None
    }
    fn confirmed_directory_snapshot(&self) -> Option<ConfirmedDirectoryObservation> {
        self.state.lock().unwrap().directory.clone()
    }
    fn close_work_snapshot(&self) -> Option<CloseWorkObservation> {
        self.state.lock().unwrap().close_work
    }
    fn cleanup_snapshot(&self) -> Option<SessionCleanupObservation> {
        self.state.lock().unwrap().cleanup
    }
}
struct Empty<T>(std::marker::PhantomData<T>);
impl<T: Send> RuntimeStream<T> for Empty<T> {
    fn next(&mut self) -> ServerFuture<'_, Option<Result<T, ServerFailure>>> {
        Box::pin(async { None })
    }
}
impl RuntimeOwnerPort for Runtime {
    fn handle(&self) -> Arc<dyn RuntimeHandlePort> {
        Arc::new(Handle(self.state.clone()))
    }
    fn subscribe(
        &self,
        _: EffectFence,
    ) -> Result<Box<dyn RuntimeStream<RuntimePresentationEvent>>, ServerFailure> {
        Ok(Box::new(Empty(std::marker::PhantomData)))
    }
    fn lifecycle_snapshot(&self) -> RuntimeLifecycle {
        self.state.lock().unwrap().lifecycle
    }
    fn metadata_snapshot(&self) -> Option<RuntimeMetadataSnapshot> {
        None
    }
    fn transition_authority(
        &mut self,
        transition: RuntimeAuthorityTransition,
    ) -> Result<ServerFuture<'_, Result<AuthorityApplied, ServerFailure>>, ServerFailure> {
        let state = self.state.clone();
        let gate = self.barrier_gate.lock().unwrap().take();
        {
            let mut state = state.lock().unwrap();
            if state.authority != transition.expected() {
                return Err(failure(ServerFailureClass::StaleAuthority));
            }
            state.authority = transition.next();
        }
        Ok(Box::pin(async move {
            if let Some(gate) = gate {
                gate.wait().await;
            }
            Ok(AuthorityApplied::new(transition))
        }))
    }
    fn retire<'a>(
        &'a mut self,
        request: RetirementRequest<'a>,
    ) -> ServerFuture<'a, Result<SessionCleanupObservation, ServerFailure>> {
        let session = self.session();
        let expected = *request
            .sessions()
            .iter()
            .find(|v| v.session() == session)
            .unwrap();
        let first = SessionTerminalResult::new(
            session,
            RuntimeTerminalStatus::Retired {
                reason: expected.first_reason(),
            },
        )
        .unwrap();
        let state = self.state.clone();
        let gate = self.retirement_gate.lock().unwrap().clone();
        let fail = self.cleanup_failed.clone();
        self.state.lock().unwrap().retire_calls += 1;
        self.state.lock().unwrap().lifecycle = RuntimeLifecycle::Retiring { terminal: first };
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.wait().await;
            }
            let result = if fail.load(Ordering::SeqCst) {
                CleanupResult::Failed(FailureClass::RuntimeFault)
            } else {
                CleanupResult::Confirmed(ConfirmedCleanupClass::ReapedAndReleased)
            };
            let observation =
                SessionCleanupObservation::new(session, expected.first_reason(), result).unwrap();
            let mut state = state.lock().unwrap();
            state.cleanup = Some(observation);
            state.lifecycle =
                RuntimeLifecycle::Finished(RuntimeCompletion::new(first, observation).unwrap());
            Ok(observation)
        })
    }
}
struct Handle(Arc<Mutex<RuntimeState>>);
fn effect_disposition(authority: RuntimeAuthorityParts, fence: EffectFence) -> EffectDisposition {
    if Some(fence.connection()) == authority.connection
        && Some(fence.grant()) == authority.grant
        && fence.session() == authority.session
        && fence.context() == authority.context
    {
        EffectDisposition::Applied
    } else {
        EffectDisposition::StaleAuthority
    }
}
impl RuntimeHandlePort for Handle {
    fn try_submit(
        &self,
        command: FencedRuntimeCommand,
    ) -> Result<ServerFuture<'_, Result<EffectDisposition, ServerFailure>>, ServerFailure> {
        let state = self.0.clone();
        // The future represents the delayed final effect, not ingress admission.
        Ok(Box::pin(async move {
            let authority = state.lock().unwrap().authority.parts();
            Ok(effect_disposition(authority, command.fence))
        }))
    }
    fn complete_native(
        &self,
        completion: spaceterm_core::native_requests::NativeCompletion,
    ) -> Result<ServerFuture<'_, Result<EffectDisposition, ServerFailure>>, ServerFailure> {
        let state = self.0.clone();
        Ok(Box::pin(async move {
            let authority = state.lock().unwrap().authority.parts();
            Ok(effect_disposition(authority, completion.fence()))
        }))
    }
    fn read_page(
        &self,
        _: spaceterm_core::content_queries::ContentPageRequest,
    ) -> Result<
        ServerFuture<'_, Result<spaceterm_core::content_queries::ContentPageResult, ServerFailure>>,
        ServerFailure,
    > {
        Err(failure(ServerFailureClass::RuntimeUnavailable))
    }
    fn read_topology(
        &self,
        _: spaceterm_core::content_queries::TopologyPageRequest,
    ) -> Result<
        ServerFuture<
            '_,
            Result<spaceterm_core::content_queries::TopologyPageResult, ServerFailure>,
        >,
        ServerFailure,
    > {
        Err(failure(ServerFailureClass::RuntimeUnavailable))
    }
    fn prepare_copy(
        &self,
        _: spaceterm_core::content_queries::CopyRequest,
    ) -> Result<
        Box<dyn RuntimeStream<spaceterm_core::content_queries::CopyStreamItem>>,
        ServerFailure,
    > {
        Err(failure(ServerFailureClass::RuntimeUnavailable))
    }
    fn find(
        &self,
        _: spaceterm_core::content_queries::FindRequest,
    ) -> Result<
        Box<dyn RuntimeStream<spaceterm_core::content_queries::FindStreamItem>>,
        ServerFailure,
    > {
        Err(failure(ServerFailureClass::RuntimeUnavailable))
    }
    fn read_graphics(
        &self,
        _: spaceterm_core::content_queries::GraphicsRequest,
    ) -> Result<Box<dyn RuntimeStream<spaceterm_core::content_queries::GraphicsChunk>>, ServerFailure>
    {
        Err(failure(ServerFailureClass::RuntimeUnavailable))
    }
}
pub struct Factory {
    pub released: Arc<AtomicUsize>,
    pub release_notice: Arc<tokio::sync::Notify>,
    pub starts: Mutex<Vec<RuntimeStartRequest>>,
    pub states: Mutex<Vec<Arc<Mutex<RuntimeState>>>>,
    pub retirement_gate: Arc<Mutex<Option<Arc<Gate>>>>,
    pub barrier_gate: Arc<Mutex<Option<Arc<Gate>>>>,
    pub cleanup_failed: Arc<AtomicBool>,
    pub abandoned: AtomicUsize,
    pub abandon_gate: Mutex<Option<Arc<Gate>>>,
    pub start_gate: Mutex<Option<Arc<Gate>>>,
}
impl RuntimeFactoryPort for Factory {
    fn start<'a>(
        &'a self,
        request: &'a RuntimeStartRequest,
    ) -> ServerFuture<'a, Result<Box<dyn RuntimeOwnerPort>, ServerFailure>> {
        self.starts.lock().unwrap().push(request.clone());
        let state = Arc::new(Mutex::new(RuntimeState {
            authority: request.authority(),
            lifecycle: RuntimeLifecycle::Running,
            directory: None,
            close_work: None,
            cleanup: None,
            retire_calls: 0,
        }));
        self.states.lock().unwrap().push(state.clone());
        let runtime = Runtime {
            released: self.released.clone(),
            release_notice: self.release_notice.clone(),
            state,
            retirement_gate: self.retirement_gate.clone(),
            barrier_gate: self.barrier_gate.clone(),
            cleanup_failed: self.cleanup_failed.clone(),
        };
        let gate = self.start_gate.lock().unwrap().clone();
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.wait().await;
            }
            Ok(Box::new(runtime) as Box<dyn RuntimeOwnerPort>)
        })
    }
}
impl RuntimeCleanupPort for Factory {
    fn abandon(
        &self,
        owner: Box<dyn RuntimeOwnerPort>,
    ) -> ServerFuture<'static, Result<SessionCleanupObservation, ServerFailure>> {
        self.abandoned.fetch_add(1, Ordering::SeqCst);
        let session = owner.session();
        let gate = self.abandon_gate.lock().unwrap().clone();
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.wait().await;
            }
            drop(owner);
            Ok(SessionCleanupObservation::new(
                session,
                RetirementReason::ServerRecovery,
                CleanupResult::Confirmed(ConfirmedCleanupClass::ReapedAndReleased),
            )
            .unwrap())
        })
    }
}
struct Prepared {
    gate: Option<Arc<Gate>>,
}
impl PreparedLaunchPort for Prepared {
    fn start(
        self: Box<Self>,
        factory: Arc<dyn RuntimeFactoryPort>,
        request: RuntimeStartRequest,
        gate: Arc<LaunchGate>,
    ) -> ServerFuture<'static, Result<Box<dyn RuntimeOwnerPort>, ServerFailure>> {
        Box::pin(async move {
            if let Some(before_process) = self.gate {
                before_process.wait().await;
            }
            gate.admit_process()?;
            factory.start(&request).await
        })
    }
}
pub struct Launch {
    pub preparations: AtomicUsize,
    pub gate: Mutex<Option<Arc<Gate>>>,
    pub before_process: Mutex<Option<Arc<Gate>>>,
    pub failure: Mutex<Option<OperationFailure>>,
}
impl LaunchPreparationPort for Launch {
    fn prepare<'a>(
        &'a self,
        _: &'a RuntimeStartRequest,
    ) -> ServerFuture<'a, Result<Box<dyn PreparedLaunchPort>, OperationFailure>> {
        self.preparations.fetch_add(1, Ordering::SeqCst);
        let gate = self.gate.lock().unwrap().clone();
        let before_process = self.before_process.lock().unwrap().clone();
        let failure = *self.failure.lock().unwrap();
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.wait().await;
            }
            if let Some(failure) = failure {
                return Err(failure);
            }
            Ok(Box::new(Prepared {
                gate: before_process,
            }) as Box<dyn PreparedLaunchPort>)
        })
    }
}

pub struct Outcomes {
    pub rejected: AtomicBool,
    pub publications: Arc<AtomicUsize>,
}
struct OutcomePermit(Arc<AtomicUsize>);
impl ReliableOutcomePermit for OutcomePermit {
    fn publish(&mut self, _: &OperationResponse) -> Result<(), ServerFailure> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
impl ReliableOutcomePort for Outcomes {
    fn reserve(
        &self,
        _: VerifiedPrincipal,
        _: OperationBinding,
    ) -> Result<Box<dyn ReliableOutcomePermit>, ServerFailure> {
        if self.rejected.load(Ordering::SeqCst) {
            Err(failure(ServerFailureClass::Capacity))
        } else {
            Ok(Box::new(OutcomePermit(self.publications.clone())))
        }
    }
}
pub struct Fixture {
    pub config: ServerConfig,
    pub dependencies: ServerDependencies,
    pub db: Arc<Mutex<Database>>,
    pub codec: Arc<Codec>,
    pub host: Arc<Host>,
    pub factory: Arc<Factory>,
    pub launch: Arc<Launch>,
    pub external: Arc<External>,
    pub observer: Arc<Observer>,
    pub key: Arc<TestKey>,
    pub pairings: Arc<Pairings>,
    pub validator: Arc<Validator>,
    pub outcomes: Arc<Outcomes>,
}
impl Fixture {
    pub fn new() -> Self {
        let directory = ServerDirectory::new(server(), b"/test-only".to_vec()).unwrap();
        let state = OrganizationState {
            server: server(),
            revision: OrganizationRevision::new(1),
            workspaces: vec![WorkspaceRecord {
                id: workspace(),
                name: Versioned {
                    version: FieldVersion::new(1),
                    value: WorkspaceName {
                        mode: NameMode::Automatic,
                        display: NameText::new("test".into()).unwrap(),
                    },
                },
                pinned_directory: Versioned {
                    version: FieldVersion::new(1),
                    value: None,
                },
                root_tab: tab(),
                creation_ordinal: CreationOrdinal::new(1),
            }],
            tabs: vec![TabRecord {
                id: tab(),
                workspace: workspace(),
                worktree: None,
                root_pane: pane(),
                creation_ordinal: CreationOrdinal::new(2),
            }],
            panes: vec![PaneRecord {
                id: pane(),
                tab: tab(),
                creation_ordinal: CreationOrdinal::new(3),
                creation: PaneCreation::Initial,
                starting_directory: directory,
                confirmed_restoration_directory: Versioned {
                    version: FieldVersion::new(1),
                    value: None,
                },
            }],
            repositories: vec![],
            worktrees: vec![],
            settings: ServerSettingsRecord {
                worktree_location: Versioned {
                    version: FieldVersion::new(1),
                    value: WorktreeLocationTemplate::new("test".into()).unwrap(),
                },
            },
            retained: None,
        };
        let marks = IdentityHighWater {
            workspace: 1,
            tab: 1,
            pane: 1,
            client: 2,
            pairing: 1,
            operation_stream: 2,
            creation_ordinal: 3,
            ..Default::default()
        };
        let recovered = RecoveredState {
            organization: state.clone(),
            store_revision: StoreRevision::new(1),
            last_epoch: ServerEpoch::new(1),
            high_water: marks,
            streams: vec![],
            operations: vec![],
            ledger: None,
            pairing: None,
            git_repositories: vec![],
            git_intents: vec![],
            git_allocations: vec![],
            native_retirements: vec![],
        };
        let db = Arc::new(Mutex::new(Database {
            recovered,
            transactions: vec![],
            next: None,
            gate: None,
            recovery_failure: None,
            log: vec![],
            close_observations: vec![],
            reject_stream_registration: false,
        }));
        let snapshot = Arc::new(
            OrganizationSnapshot::new(
                ServerEpoch::new(1),
                state,
                vec![PaneRuntimeRecord {
                    pane: pane(),
                    session_generation: SessionGeneration::new(0),
                    phase: SessionPhase::NotStarted,
                    control_generation: ControlGeneration::new(0),
                    control: ControlRecord::Unclaimed,
                }],
            )
            .unwrap(),
        );
        let codec = Arc::new(Codec::default());
        let validator = Arc::new(Validator {
            rejected: AtomicBool::new(false),
        });
        let core = CorePorts {
            reducer: Box::new(Reducer {
                snapshot,
                staged: None,
                high_water: marks,
                codec: codec.clone(),
                log: db.clone(),
            }),
            ledger: Box::new(Ledger {
                entries: BTreeMap::new(),
                streams: [stream(1), stream(2)]
                    .into_iter()
                    .map(|stream| {
                        (
                            stream,
                            StreamRecord::new(
                                StreamBinding::from_stream(stream),
                                StreamPurpose::Ordinary,
                                StreamStatus::Active,
                                0,
                                0,
                                None,
                            )
                            .unwrap(),
                        )
                    })
                    .collect(),
                staged: None,
                staged_streams: None,
                codec: codec.clone(),
                log: db.clone(),
            }),
            codec: codec.clone(),
            validator: validator.clone(),
        };
        let host = Arc::new(Host {
            cleanup: Mutex::new(vec![]),
            requests: Mutex::new(vec![]),
            reject_start: AtomicBool::new(false),
        });
        let key = TestKey::new(41);
        let pairings = Arc::new(Pairings {
            key: TestKey::new(42),
            active: AtomicBool::new(true),
            clients: Mutex::new([(local(1), true), (local(2), true)].into_iter().collect()),
        });
        let factory = Arc::new(Factory {
            released: Arc::new(AtomicUsize::new(0)),
            release_notice: Arc::new(tokio::sync::Notify::new()),
            starts: Mutex::new(vec![]),
            states: Mutex::new(vec![]),
            retirement_gate: Arc::new(Mutex::new(None)),
            barrier_gate: Arc::new(Mutex::new(None)),
            cleanup_failed: Arc::new(AtomicBool::new(false)),
            abandoned: AtomicUsize::new(0),
            abandon_gate: Mutex::new(None),
            start_gate: Mutex::new(None),
        });
        let launch = Arc::new(Launch {
            preparations: AtomicUsize::new(0),
            gate: Mutex::new(None),
            before_process: Mutex::new(None),
            failure: Mutex::new(None),
        });
        let external = Arc::new(External {
            reads_cancelled: AtomicUsize::new(0),
            finished: AtomicUsize::new(0),
            gate: Mutex::new(None),
        });
        let observer = Arc::new(Observer {
            admitted: AtomicUsize::new(0),
            directories: AtomicUsize::new(0),
            pairing_commits: AtomicUsize::new(0),
            pairings: pairings.clone(),
        });
        let outcomes = Arc::new(Outcomes {
            rejected: AtomicBool::new(false),
            publications: Arc::new(AtomicUsize::new(0)),
        });
        let config = ServerConfig {
            limits: ServerLimits::default(),
            host: ServerHostContext {
                authority: host.clone(),
            },
            core,
            launch: launch.clone(),
            cleanup: factory.clone(),
            extensions: observer.clone(),
            initial_appearance: appearance(),
            outcomes: outcomes.clone(),
        };
        let dependencies = ServerDependencies {
            store: Box::new(Store(db.clone())),
            credentials: key.clone(),
            pairing: pairings.clone(),
            runtime: factory.clone(),
            external_work: external.clone(),
            host_management: host.clone(),
            clock: Arc::new(Clock),
        };
        Self {
            config,
            dependencies,
            db,
            codec,
            host,
            factory,
            launch,
            external,
            observer,
            key,
            pairings,
            validator,
            outcomes,
        }
    }
    pub fn principal(&self, n: u64, generation: u64) -> VerifiedPrincipal {
        principal(&self.pairings, self.key.clone(), n, generation, 2)
    }
}
pub async fn connect(handle: &ServerHandle, principal: VerifiedPrincipal) {
    handle.register(principal).await.unwrap();
    handle
        .reconcile(principal, handle.snapshot().state().revision)
        .await
        .unwrap();
}
pub fn request(
    codec: &Codec,
    n: u64,
    sequence: u64,
    command: Command,
) -> CanonicalOperationRequest {
    CanonicalOperationRequest::new(operation(n, sequence), command, codec).unwrap()
}
pub async fn acquire(
    handle: &ServerHandle,
    principal: VerifiedPrincipal,
    codec: &Codec,
) -> ControlGrant {
    let result = handle
        .submit(
            principal,
            request(
                codec,
                principal.client().local(),
                1,
                Command::CompareAcquire(CompareAcquireRequest {
                    epoch: principal.epoch(),
                    pane: pane(),
                    client: principal.client(),
                    expected_generation: ControlGeneration::new(0),
                }),
            ),
        )
        .await
        .unwrap();
    let OperationResponse::Completed {
        outcome: Outcome::ControlGranted(grant),
        ..
    } = result
    else {
        panic!("control grant")
    };
    grant.grant()
}
pub async fn start(
    handle: &ServerHandle,
    principal: VerifiedPrincipal,
    codec: &Codec,
    grant: ControlGrant,
) -> SessionKey {
    start_at(handle, principal, codec, grant, 2).await
}
pub async fn start_at(
    handle: &ServerHandle,
    principal: VerifiedPrincipal,
    codec: &Codec,
    grant: ControlGrant,
    sequence: u64,
) -> SessionKey {
    let session = SessionKey {
        epoch: principal.epoch(),
        pane: pane(),
        generation: handle.snapshot().runtime()[0].session_generation,
    };
    let fence = EffectFence::new(
        session,
        grant,
        principal.binding(),
        ContextGeneration::new(1),
        None,
    )
    .unwrap();
    let intent = StartupIntent::new(
        operation(principal.client().local(), sequence),
        fence,
        StartupSource::ExplicitSelection,
    )
    .unwrap();
    handle
        .submit(
            principal,
            request(
                codec,
                principal.client().local(),
                sequence,
                Command::BeginStartup(intent),
            ),
        )
        .await
        .unwrap();
    let grid = spaceterm_core::geometry::MeasuredGrid::new(
        37,
        113,
        spaceterm_core::geometry::CellPixels::new(9, 18).unwrap(),
        spaceterm_core::geometry::SUPPORTED_GRID_LIMITS,
    )
    .unwrap();
    let result = handle
        .submit(
            principal,
            request(
                codec,
                principal.client().local(),
                sequence + 1,
                Command::CompleteMeasuredStartup(MeasuredStartupMeasurement {
                    intent: intent.id(),
                    fence,
                    grid,
                }),
            ),
        )
        .await
        .unwrap();
    let OperationResponse::Completed {
        outcome: Outcome::SessionStarted(session),
        ..
    } = result
    else {
        panic!("start admitted")
    };
    let mut snapshots = handle.subscribe_snapshot();
    while !snapshots.borrow().runtime().iter().any(
        |v| matches!(v.phase,SessionPhase::Running{generation} if generation==session.generation),
    ) {
        snapshots.changed().await.unwrap();
    }
    session
}
pub async fn stop(handle: &ServerHandle, host: Arc<Host>) -> Result<ShutdownReport, ServerFailure> {
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        handle.shutdown(TrustedShutdownRequest { authority: host }),
    )
    .await
    .expect("bounded test shutdown")
}

pub fn git_intent(
    binding: OperationBinding,
    phase: spaceterm_core::worktrees::intent::GitIntentPhase,
) -> spaceterm_core::worktrees::intent::GitWriteIntent {
    use spaceterm_core::worktrees::intent::*;
    let directory = ServerDirectory::new(server(), b"/test-only/repository".to_vec()).unwrap();
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
                name: BranchName::new("test".into()).unwrap(),
            },
            target: CreationTargetObservation::Absent,
            template_version: None,
        },
        phase,
        assessment_digest: GitAssessmentDigest::from_bytes([13; 32]),
        affected: RetainedRemovalScope::empty(server()),
        allocation: binding,
        external_result: None,
    })
    .unwrap()
}

pub fn git_progress(
    intent: &spaceterm_core::worktrees::intent::GitWriteIntent,
    store: StoreRevision,
) -> PendingExternalProgress {
    use spaceterm_core::worktrees::intent::GitIntentPhase;
    let phase = match intent.phase() {
        GitIntentPhase::Prepared => ExternalIntentPhase::Prepared,
        GitIntentPhase::Started => ExternalIntentPhase::Started,
        GitIntentPhase::ExternalFinished => ExternalIntentPhase::ExternalFinished,
    };
    PendingExternalProgress::new(PendingExternalProgressParts {
        binding: intent.binding(),
        effect: ExternalEffectKind::GitWrite,
        state: ExternalProgressState::RecordedIntent {
            reference: ExternalIntentReference::new(ExternalIntentReferenceParts {
                binding: intent.binding(),
                scope: ExternalIntentScope::Git(intent.parts().repository.scope()),
                phase,
                recorded_at: store,
            })
            .unwrap(),
            retirement: None,
        },
    })
    .unwrap()
}
