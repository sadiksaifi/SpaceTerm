use super::*;
use crate::{
    operations::{OperationTicket, OperationTracker},
    pane_channel::*,
    projection::ProjectionBudgets,
};
use spaceterm_core::geometry::{GridLimits, SUPPORTED_GRID_LIMITS};
use spaceterm_terminal::runtime_contracts::FencedRuntimeCommand;
use std::{collections::VecDeque, sync::Weak};

/// Typed frozen-port fixture until server-core's generic runtime/content converters
/// are installed. Authentication tests use FakeServer's separate real codec route.
pub struct FakePaneConnection {
    server: Arc<FakeServer>,
    binding: PairedServerBinding,
    tracker: OperationTracker,
    serialized: tokio::sync::Mutex<()>,
    info: Mutex<ConnectedInfo>,
    channels: Mutex<Weak<PaneChannels>>,
    effects: Mutex<VecDeque<(EffectPermit, QueuedEffect)>>,
    effect_requests: AtomicU64,
    reads: Mutex<VecDeque<VecDeque<Result<PaneReadItem, ClientFailure>>>>,
    cancelled: Mutex<Vec<StartupIntent>>,
    pub grid_limits: Mutex<GridLimits>,
    intent_response_gates: Mutex<VecDeque<Arc<tokio::sync::Semaphore>>>,
}
enum QueuedEffect {
    Command(Box<FencedRuntimeCommand>),
    Copy(spaceterm_terminal::InputModifiers),
}
impl QueuedEffect {
    fn bytes(&self) -> usize {
        // Conservative fixture accounting includes the maximum engine-free command.
        let Self::Command(command) = self else {
            return 4096;
        };
        match &command.command {
            RuntimeEffect::RequestPaste(text) => text.as_str().len() + 4096,
            RuntimeEffect::InputMethodCommit(text) => text.as_str().len() + 4096,
            RuntimeEffect::CompleteNativeRequest(completion) => match completion.kind() {
                spaceterm_core::native_requests::NativeCompletionKind::ClipboardText(text) => {
                    text.as_str().len() + 4096
                }
                _ => 4096,
            },
            _ => 4096,
        }
    }
}
impl FakePaneConnection {
    pub async fn new(
        server: Arc<FakeServer>,
        client: u64,
        major: u32,
    ) -> Result<Arc<Self>, ClientFailure> {
        let journal = journal::controller(&server, client).await?;
        let info = server.connected_info(client, 1, 1, major)?;
        server.activate_connection(info.binding)?;
        Ok(Arc::new(Self {
            binding: server.binding(client)?,
            server,
            tracker: OperationTracker::new(journal),
            serialized: tokio::sync::Mutex::new(()),
            info: Mutex::new(info),
            channels: Mutex::new(Weak::new()),
            effects: Mutex::new(VecDeque::new()),
            effect_requests: AtomicU64::new(0),
            reads: Mutex::new(VecDeque::new()),
            cancelled: Mutex::new(vec![]),
            grid_limits: Mutex::new(SUPPORTED_GRID_LIMITS),
            intent_response_gates: Mutex::new(VecDeque::new()),
        }))
    }
    pub fn channels(
        self: &Arc<Self>,
        budgets: ProjectionBudgets,
    ) -> Result<Arc<PaneChannels>, ClientFailure> {
        let channels = Arc::new(PaneChannels::new(self.clone(), budgets)?);
        *self.channels.lock().map_err(|_| invalid_peer())? = Arc::downgrade(&channels);
        self.publish()?;
        Ok(channels)
    }
    pub fn reconnect(&self, generation: u64, attempt: u64) -> Result<(), ClientFailure> {
        let mut effects = self.effects.lock().map_err(|_| invalid_peer())?;
        effects.clear();
        self.effect_requests.store(0, Ordering::Release);
        let old = *self.info.lock().map_err(|_| invalid_peer())?;
        *self.info.lock().map_err(|_| invalid_peer())? =
            self.server
                .connected_info(old.client.local(), generation, attempt, old.major)?;
        self.server
            .activate_connection(self.info.lock().map_err(|_| invalid_peer())?.binding)?;
        self.publish()
    }
    pub fn publish(&self) -> Result<(), ClientFailure> {
        if let Some(channels) = self.channels.lock().map_err(|_| invalid_peer())?.upgrade() {
            let mut info = *self.info.lock().map_err(|_| invalid_peer())?;
            let snapshot = self.server.replica()?;
            info.organization_revision = snapshot.cursor.revision;
            *self.info.lock().map_err(|_| invalid_peer())? = info;
            channels.publish(info, snapshot)?;
        }
        Ok(())
    }
    pub async fn submit(
        &self,
        command: Command,
        workspace: Option<WorkspaceId>,
    ) -> Result<OperationTicket, ClientFailure> {
        let _serialized = self.serialized.lock().await;
        self.submit_locked(command, workspace).await
    }
    async fn submit_locked(
        &self,
        command: Command,
        workspace: Option<WorkspaceId>,
    ) -> Result<OperationTicket, ClientFailure> {
        if let Some(channels) = self.channels.lock().map_err(|_| invalid_peer())?.upgrade() {
            let info = *self.info.lock().map_err(|_| invalid_peer())?;
            for (pane, context) in channels.contexts(info.server)? {
                self.server.update_context(info.binding, pane, context)?;
            }
        }
        let ticket = self
            .tracker
            .submit(&self.binding, command, workspace, self.server.as_ref())
            .await?;
        let lookup = self.server.lookup(ticket.binding()).await?;
        if !self
            .server
            .faults
            .drop_next_outcome
            .swap(false, Ordering::AcqRel)
        {
            self.tracker
                .resolve_bound(ticket.binding(), lookup, self.server.as_ref())
                .await?;
        }
        self.publish()?;
        Ok(ticket)
    }
    pub fn queue_read(&self, items: Vec<PaneReadItem>) -> Result<(), ClientFailure> {
        self.queue_read_results(items.into_iter().map(Ok).collect())
    }
    /// Scripted failures are confined to this test-only missing-provider fixture.
    pub fn queue_read_results(
        &self,
        items: Vec<Result<PaneReadItem, ClientFailure>>,
    ) -> Result<(), ClientFailure> {
        let mut reads = self.reads.lock().map_err(|_| invalid_peer())?;
        if reads.len() >= 4 || items.len() > 128 {
            return Err(resource_limit());
        }
        reads.push_back(items.into());
        Ok(())
    }
    /// Simulates final wire dequeue with the original permits, never new fences.
    pub fn flush_effects(&self) -> Result<Vec<FencedRuntimeCommand>, ClientFailure> {
        let mut effects = self.effects.lock().map_err(|_| invalid_peer())?;
        let mut sent = Vec::new();
        for (permit, effect) in std::mem::take(&mut *effects) {
            if !permit.is_current() {
                continue;
            }
            match effect {
                QueuedEffect::Command(command) => sent.push(*command),
                copy => effects.push_back((permit, copy)),
            }
        }
        Ok(sent)
    }
    pub fn flush_copies(
        &self,
    ) -> Result<Vec<(EffectFence, spaceterm_terminal::InputModifiers)>, ClientFailure> {
        let mut effects = self.effects.lock().map_err(|_| invalid_peer())?;
        let mut sent = Vec::new();
        for (permit, effect) in std::mem::take(&mut *effects) {
            if !permit.is_current() {
                continue;
            }
            match effect {
                QueuedEffect::Copy(modifiers) => sent.push((permit.fence(), modifiers)),
                command => effects.push_back((permit, command)),
            }
        }
        Ok(sent)
    }
    fn queue_effect(
        &self,
        permit: EffectPermit,
        effect: QueuedEffect,
    ) -> Result<EffectAdmission, ClientFailure> {
        if !permit.is_current() {
            return Err(stale());
        }
        let mut effects = self.effects.lock().map_err(|_| invalid_peer())?;
        if !permit.is_current() {
            return Err(stale());
        }
        if effects.len() >= spaceterm_protocol::limits::MAX_EGRESS_ENVELOPES
            || effects
                .iter()
                .map(|(_, effect)| effect.bytes())
                .sum::<usize>()
                .checked_add(effect.bytes())
                .is_none_or(|bytes| bytes > spaceterm_protocol::limits::MAX_EGRESS_BYTES)
        {
            return Err(resource_limit());
        }
        let admission = self.effect_admission(&permit)?;
        effects.push_back((permit, effect));
        Ok(admission)
    }
    fn effect_admission(&self, permit: &EffectPermit) -> Result<EffectAdmission, ClientFailure> {
        let request = self
            .effect_requests
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .map_err(|_| exhausted())?
            + 1;
        Ok(EffectAdmission::Correlated(EffectCorrelation::new(
            request,
            permit.fence(),
        )?))
    }
    /// Delays one completed server intent reply without blocking later requests.
    pub fn delay_next_intent_response(&self) -> Result<Arc<tokio::sync::Semaphore>, ClientFailure> {
        let mut gates = self
            .intent_response_gates
            .lock()
            .map_err(|_| invalid_peer())?;
        if gates.len() >= 4 {
            return Err(resource_limit());
        }
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        gates.push_back(Arc::clone(&gate));
        Ok(gate)
    }
    pub fn cancelled_intents(&self) -> Result<Vec<StartupIntent>, ClientFailure> {
        Ok(self.cancelled.lock().map_err(|_| invalid_peer())?.clone())
    }
    pub fn journal(&self) -> &Arc<crate::journal::JournalController> {
        self.tracker.journal()
    }
}
impl PaneConnectionPort for FakePaneConnection {
    fn grid_limits(&self, _info: ConnectedInfo) -> Result<GridLimits, ClientFailure> {
        Ok(*self.grid_limits.lock().map_err(|_| invalid_peer())?)
    }
    fn explicit_use(&self, request: PaneUseRequest) -> ClientFuture<'_, PaneUseReply> {
        Box::pin(async move {
            let _serialized = self.serialized.lock().await;
            if request.info.binding != self.info.lock().map_err(|_| invalid_peer())?.binding {
                return Err(stale());
            }
            if request.runtime.control == ControlRecord::Unclaimed {
                self.submit_locked(
                    Command::CompareAcquire(CompareAcquireRequest {
                        epoch: request.info.epoch,
                        pane: request.runtime.pane,
                        client: request.info.client,
                        expected_generation: request.runtime.control_generation,
                    }),
                    Some(request.workspace),
                )
                .await?;
            }
            let snapshot = self.server.snapshot()?;
            let runtime = snapshot
                .runtime()
                .iter()
                .find(|runtime| runtime.pane == request.runtime.pane)
                .ok_or_else(stale)?;
            let ControlRecord::Held { client, generation } = runtime.control else {
                return Err(stale());
            };
            if client != request.info.client {
                return Err(stale());
            }
            let fence = EffectFence::new(
                SessionKey {
                    pane: runtime.pane,
                    epoch: snapshot.epoch(),
                    generation: runtime.session_generation,
                },
                ControlGrant::new(runtime.pane, client, generation).map_err(|_| invalid_peer())?,
                request.info.binding,
                request.context,
                None,
            )
            .map_err(|_| invalid_peer())?;
            if matches!(
                runtime.phase,
                SessionPhase::Running { .. } | SessionPhase::Starting { .. }
            ) {
                return Ok(PaneUseReply::ControlReady(
                    GrantedControl::new(snapshot.epoch(), fence.grant())
                        .map_err(|_| invalid_peer())?,
                ));
            }
            let state = self.tracker.journal().snapshot()?;
            let stream = state
                .bindings()
                .iter()
                .find(|saved| saved.binding().is_ok_and(|binding| binding == self.binding))
                .ok_or_else(stale)?
                .streams()
                .parts()
                .ordinary;
            let id = OperationId::new(
                stream,
                OperationSequence::new(
                    state
                        .stream(stream)
                        .ok_or_else(stale)?
                        .high_water
                        .checked_add(1)
                        .ok_or_else(exhausted)?,
                )
                .map_err(|_| exhausted())?,
            );
            let intent =
                StartupIntent::new(id, fence, request.source).map_err(|_| invalid_peer())?;
            let ticket = self
                .submit_locked(Command::BeginStartup(intent), Some(request.workspace))
                .await?;
            if !matches!(*ticket.status().borrow(), crate::operations::OperationStatus::Completed { outcome: Outcome::StartupAwaitingMeasurement { intent: received }, .. } if received == id)
            {
                return Err(stale());
            }
            let gate = self
                .intent_response_gates
                .lock()
                .map_err(|_| invalid_peer())?
                .pop_front();
            drop(_serialized);
            if let Some(gate) = gate {
                gate.acquire().await.map_err(|_| stale())?.forget();
            }
            Ok(PaneUseReply::AwaitingMeasurement(intent))
        })
    }
    fn complete_intent(
        &self,
        workspace: WorkspaceId,
        measurement: MeasuredStartupMeasurement,
    ) -> ClientFuture<'_, OperationTicket> {
        Box::pin(async move {
            self.submit(
                Command::CompleteMeasuredStartup(measurement),
                Some(workspace),
            )
            .await
        })
    }
    fn cancel_intent(&self, intent: StartupIntent) {
        if let Ok(mut cancelled) = self.cancelled.lock() {
            cancelled.push(intent);
        }
        if let Ok(mut domain) = self.server.domain.lock() {
            domain.cancel_intent(intent);
        }
    }
    fn take_over(
        &self,
        workspace: WorkspaceId,
        request: TakeOverRequest,
    ) -> ClientFuture<'_, OperationTicket> {
        Box::pin(async move {
            self.submit(Command::TakeOver(request), Some(workspace))
                .await
        })
    }
    fn try_effect(
        &self,
        permit: EffectPermit,
        command: FencedRuntimeCommand,
    ) -> Result<EffectAdmission, ClientFailure> {
        self.queue_effect(permit, QueuedEffect::Command(Box::new(command)))
    }
    fn try_copy(
        &self,
        permit: EffectPermit,
        modifiers: spaceterm_terminal::InputModifiers,
    ) -> Result<EffectAdmission, ClientFailure> {
        self.queue_effect(permit, QueuedEffect::Copy(modifiers))
    }
    fn read(
        &self,
        permit: EffectPermit,
        _request: PaneReadRequest,
    ) -> ClientFuture<'_, Box<dyn PaneReadSource>> {
        Box::pin(async move {
            if !permit.is_current() {
                return Err(stale());
            }
            let items = self
                .reads
                .lock()
                .map_err(|_| invalid_peer())?
                .pop_front()
                .ok_or_else(stale)?;
            Ok(Box::new(ScriptedRead(items)) as Box<dyn PaneReadSource>)
        })
    }
}
struct ScriptedRead(VecDeque<Result<PaneReadItem, ClientFailure>>);
impl PaneReadSource for ScriptedRead {
    fn next(&mut self) -> ClientFuture<'_, Option<PaneReadItem>> {
        Box::pin(async { self.0.pop_front().transpose() })
    }
}
