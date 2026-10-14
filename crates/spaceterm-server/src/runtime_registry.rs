//! Retained runtime ownership is separate from connection handles. Each owner
//! task serializes its bounded lifecycle lane while the actor continues serving
//! disconnect, revocation and store completion messages.

use crate::adapters::terminal::{RetainedRuntimeObservations, RuntimeCleanupPort};
use crate::contracts::*;
use crate::failure::failure;
use spaceterm_core::{authority::*, identity::PaneId, retirement::*, runtime_authority::*};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::{mpsc, oneshot, watch};

#[derive(Clone)]
pub enum OwnedRetirement {
    Operation(Arc<RetirementDescriptor>),
    Native(Arc<NativeRetirementDescriptor>),
}
impl OwnedRetirement {
    pub fn request(&self) -> RetirementRequest<'_> {
        match self {
            Self::Operation(v) => RetirementRequest::Operation(v),
            Self::Native(v) => RetirementRequest::Native(v),
        }
    }
}

pub type PresentationSubscription = Box<dyn RuntimeStream<RuntimePresentationEvent>>;

pub type CleanupReply = (
    SessionKey,
    oneshot::Receiver<Result<SessionCleanupObservation, ServerFailure>>,
);

enum OwnerRequest {
    Authority(
        Box<RuntimeAuthorityTransition>,
        oneshot::Sender<Result<AuthorityApplied, ServerFailure>>,
    ),
    Retire(
        OwnedRetirement,
        oneshot::Sender<Result<SessionCleanupObservation, ServerFailure>>,
    ),
    Inspect(oneshot::Sender<()>),
}
struct SubscriptionRequest {
    fence: EffectFence,
    reply: oneshot::Sender<Result<Box<dyn RuntimeStream<RuntimePresentationEvent>>, ServerFailure>>,
}
struct Slot {
    subscriptions: mpsc::Sender<SubscriptionRequest>,
    sender: mpsc::Sender<OwnerRequest>,
    handle: Arc<dyn RuntimeHandlePort>,
    authority: RuntimeAuthority,
    paused: bool,
    retiring: bool,
    first_reason: Option<spaceterm_core::organization::RetirementReason>,
    observations: watch::Receiver<RetainedRuntimeObservations>,
    join: tokio::task::JoinHandle<Result<(), ServerFailure>>,
}

pub struct RuntimeRegistry {
    slots: BTreeMap<SessionKey, Slot>,
    evidence: BTreeMap<SessionKey, SessionCleanupObservation>,
    ceiling: usize,
    joining: usize,
    abandoned: Vec<tokio::task::JoinHandle<Result<SessionCleanupObservation, ServerFailure>>>,
    cleanup: Arc<dyn RuntimeCleanupPort>,
}
impl RuntimeRegistry {
    pub fn new(
        ceiling: usize,
        cleanup: Arc<dyn RuntimeCleanupPort>,
    ) -> Result<Self, ServerFailure> {
        if ceiling == 0 || ceiling > 64 {
            return Err(failure(ServerFailureClass::Capacity));
        }
        Ok(Self {
            slots: BTreeMap::new(),
            evidence: BTreeMap::new(),
            ceiling,
            joining: 0,
            abandoned: Vec::new(),
            cleanup,
        })
    }
    pub fn has_capacity(&self, starting: usize) -> bool {
        self.slots
            .len()
            .checked_add(self.joining)
            .and_then(|n| n.checked_add(self.abandoned.len()))
            .and_then(|n| n.checked_add(starting))
            .is_some_and(|n| n < self.ceiling)
    }
    pub fn contains_pane(&self, pane: PaneId) -> bool {
        self.slots.keys().any(|v| v.pane == pane)
    }
    pub fn sessions(&self) -> Vec<SessionKey> {
        self.slots.keys().copied().collect()
    }
    pub fn retirement_sessions(
        &self,
        reason: spaceterm_core::organization::RetirementReason,
    ) -> Result<Vec<RetiredSession>, ServerFailure> {
        self.slots
            .iter()
            .map(|(session, slot)| {
                let observations = slot.observations.borrow();
                let first_reason = slot
                    .first_reason
                    .or(observations
                        .lifecycle
                        .terminal_result()
                        .map(|v| v.retirement_reason()))
                    .unwrap_or(reason);
                let cleanup = self.evidence.get(session).copied().or(observations.cleanup);
                let state = match cleanup.map(|v| v.result()) {
                    Some(CleanupResult::Confirmed(value)) => {
                        RetirementCleanupState::Confirmed(value)
                    }
                    Some(CleanupResult::Failed(value)) => RetirementCleanupState::Failed(value),
                    None => RetirementCleanupState::AwaitingEvidence,
                };
                RetiredSession::new(*session, first_reason, state)
                    .map_err(|_| failure(ServerFailureClass::CleanupIncomplete))
            })
            .collect()
    }
    /// Remove only an owner whose exact retained result proves cleanup. Capacity
    /// stays charged through the task join, not merely through its map removal.
    pub fn release(
        &mut self,
        session: SessionKey,
    ) -> Result<Option<ServerFuture<'static, Result<(), ServerFailure>>>, ServerFailure> {
        let Some(slot) = self.slots.get(&session) else {
            return Ok(None);
        };
        let confirmed = self
            .evidence
            .get(&session)
            .copied()
            .or(slot.observations.borrow().cleanup);
        if !confirmed.is_some_and(|v| {
            v.session() == session && matches!(v.result(), CleanupResult::Confirmed(_))
        }) {
            return Ok(None);
        }
        let slot = self
            .slots
            .remove(&session)
            .ok_or_else(|| failure(ServerFailureClass::CleanupIncomplete))?;
        self.joining += 1;
        drop(slot.sender);
        drop(slot.subscriptions);
        Ok(Some(Box::pin(async move {
            slot.join
                .await
                .map_err(|_| failure(ServerFailureClass::CleanupIncomplete))?
        })))
    }
    pub fn joined(&mut self, session: SessionKey) {
        self.joining = self.joining.saturating_sub(1);
        self.evidence.remove(&session);
    }
    pub fn joins_pending(&self) -> bool {
        self.joining != 0
    }
    pub fn insert(
        &mut self,
        owner: Box<dyn RuntimeOwnerPort>,
        authority: RuntimeAuthority,
    ) -> Result<(), ServerFailure> {
        let session = owner.session();
        if session != authority.session()
            || !self.has_capacity(0)
            || self.contains_pane(session.pane)
        {
            let cleanup = self.cleanup.clone();
            self.abandoned.push(tokio::spawn(async move {
                let result = cleanup.abandon(owner).await?;
                if result.session() != session
                    || !matches!(result.result(), CleanupResult::Confirmed(_))
                {
                    return Err(failure(ServerFailureClass::CleanupIncomplete));
                }
                Ok(result)
            }));
            return Err(failure(ServerFailureClass::Capacity));
        }
        let handle = owner.handle();
        let (observations, received) =
            watch::channel(RetainedRuntimeObservations::capture(owner.as_ref()));
        let (sender, receiver) = mpsc::channel(8);
        let cleanup = self.cleanup.clone();
        let (subscriptions, subscription_requests) = mpsc::channel(4);
        let join = tokio::spawn(owner_loop(
            owner,
            receiver,
            subscription_requests,
            observations,
            cleanup,
        ));
        self.slots.insert(
            session,
            Slot {
                subscriptions,
                sender,
                handle,
                authority,
                paused: true,
                retiring: false,
                first_reason: None,
                observations: received,
                join,
            },
        );
        Ok(())
    }
    pub fn pause_pane(&mut self, pane: PaneId) {
        for (key, slot) in &mut self.slots {
            if key.pane == pane {
                slot.paused = true;
            }
        }
    }
    pub fn pause_all(&mut self) {
        for slot in self.slots.values_mut() {
            slot.paused = true;
        }
    }
    pub fn pause_connection(&mut self, binding: ConnectionBinding) {
        for slot in self.slots.values_mut() {
            if slot.authority.parts().connection == Some(binding) {
                slot.paused = true;
            }
        }
    }
    pub fn lifecycle(
        &self,
        session: SessionKey,
    ) -> Option<spaceterm_core::runtime_authority::RuntimeLifecycle> {
        self.slots
            .get(&session)
            .map(|slot| slot.observations.borrow().lifecycle)
    }
    pub fn is_retiring(&self, session: SessionKey) -> bool {
        self.slots.get(&session).is_some_and(|slot| slot.retiring)
    }
    pub fn authority(&self, session: SessionKey) -> Option<RuntimeAuthority> {
        self.slots.get(&session).map(|v| v.authority)
    }
    pub fn transition(
        &mut self,
        session: SessionKey,
        next: RuntimeAuthority,
    ) -> Result<oneshot::Receiver<Result<AuthorityApplied, ServerFailure>>, ServerFailure> {
        let slot = self
            .slots
            .get_mut(&session)
            .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
        slot.paused = true;
        let transition = RuntimeAuthorityTransition::new(slot.authority, next)
            .map_err(|_| failure(ServerFailureClass::StaleAuthority))?;
        let (reply, result) = oneshot::channel();
        slot.sender
            .try_send(OwnerRequest::Authority(Box::new(transition), reply))
            .map_err(|_| failure(ServerFailureClass::Capacity))?;
        // The expected authority of subsequent ordered transitions is this next
        // value. Effects remain paused until the exact worker reply is installed.
        slot.authority = next;
        Ok(result)
    }
    pub fn authority_applied(&mut self, applied: AuthorityApplied) -> Result<(), ServerFailure> {
        let next = applied.transition().next();
        let slot = self
            .slots
            .get_mut(&next.session())
            .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
        if slot.authority == next && !slot.retiring {
            slot.paused = false;
        }
        Ok(())
    }
    pub fn access(
        &self,
        binding: ConnectionBinding,
        session: SessionKey,
    ) -> Result<crate::api::RuntimeAccess, ServerFailure> {
        let slot = self
            .slots
            .get(&session)
            .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
        let parts = slot.authority.parts();
        if slot.paused
            || slot.retiring
            || parts.connection != Some(binding)
            || parts
                .grant
                .is_none_or(|grant| grant.client() != binding.client)
        {
            return Err(failure(ServerFailureClass::StaleAuthority));
        }
        Ok(crate::api::RuntimeAccess {
            authority: slot.authority,
            handle: slot.handle.clone(),
        })
    }
    pub fn subscribe(
        &self,
        binding: ConnectionBinding,
        fence: EffectFence,
    ) -> Result<ServerFuture<'static, Result<PresentationSubscription, ServerFailure>>, ServerFailure>
    {
        let access = self.access(binding, fence.session())?;
        let parts = access.authority.parts();
        if fence.connection() != binding
            || Some(fence.grant()) != parts.grant
            || fence.context() != parts.context
        {
            return Err(failure(ServerFailureClass::StaleAuthority));
        }
        let slot = self
            .slots
            .get(&fence.session())
            .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
        let (reply, result) = oneshot::channel();
        slot.subscriptions
            .try_send(SubscriptionRequest { fence, reply })
            .map_err(|_| failure(ServerFailureClass::Capacity))?;
        Ok(Box::pin(async move {
            result
                .await
                .map_err(|_| failure(ServerFailureClass::RuntimeUnavailable))?
        }))
    }
    pub fn submit(
        &self,
        binding: ConnectionBinding,
        command: spaceterm_terminal::runtime_contracts::FencedRuntimeCommand,
    ) -> Result<ServerFuture<'static, Result<EffectDisposition, ServerFailure>>, ServerFailure>
    {
        let slot = self
            .slots
            .get(&command.fence.session())
            .ok_or_else(|| failure(ServerFailureClass::StaleAuthority))?;
        let parts = slot.authority.parts();
        if slot.paused
            || slot.retiring
            || command.fence.connection() != binding
            || parts.connection != Some(binding)
            || parts.grant != Some(command.fence.grant())
            || parts.context != command.fence.context()
        {
            return Err(failure(ServerFailureClass::StaleAuthority));
        }
        let handle = slot.handle.clone();
        Ok(Box::pin(async move { handle.try_submit(command)?.await }))
    }
    pub fn observations(&self) -> Vec<(SessionKey, RetainedRuntimeObservations)> {
        self.slots
            .iter()
            .map(|(key, slot)| (*key, slot.observations.borrow().clone()))
            .collect()
    }
    pub fn inspect(&self) -> ServerFuture<'static, Result<(), ServerFailure>> {
        let senders: Vec<_> = self
            .slots
            .values()
            .map(|slot| slot.sender.clone())
            .collect();
        Box::pin(async move {
            let mut tasks = tokio::task::JoinSet::new();
            for sender in senders {
                tasks.spawn(async move {
                    let (reply, result) = oneshot::channel();
                    sender
                        .send(OwnerRequest::Inspect(reply))
                        .await
                        .map_err(|_| failure(ServerFailureClass::RuntimeUnavailable))?;
                    result
                        .await
                        .map_err(|_| failure(ServerFailureClass::RuntimeUnavailable))
                });
            }
            let mut first_error = None;
            while let Some(result) = tasks.join_next().await {
                if let Err(error) = result
                    .map_err(|_| failure(ServerFailureClass::RuntimeUnavailable))
                    .and_then(|value| value)
                {
                    first_error.get_or_insert(error);
                }
            }
            first_error.map_or(Ok(()), Err)
        })
    }
    pub fn retire(
        &mut self,
        request: OwnedRetirement,
        starting: &[SessionKey],
    ) -> Result<Vec<CleanupReply>, ServerFailure> {
        let mut replies = Vec::new();
        for retained in request.request().sessions() {
            let key = retained.session();
            if self.evidence.contains_key(&key) {
                continue;
            }
            let Some(slot) = self.slots.get_mut(&key) else {
                if starting.contains(&key) {
                    continue;
                }
                return Err(failure(ServerFailureClass::CleanupIncomplete));
            };
            slot.paused = true;
            if slot.retiring {
                continue;
            }
            let (reply, result) = oneshot::channel();
            slot.sender
                .try_send(OwnerRequest::Retire(request.clone(), reply))
                .map_err(|_| failure(ServerFailureClass::CleanupIncomplete))?;
            slot.retiring = true;
            slot.first_reason = Some(retained.first_reason());
            replies.push((key, result));
        }
        Ok(replies)
    }
    /// Only the actor can prove that its consumed launch completed without ever
    /// crossing the process gate. This is not absence-of-handles inference.
    pub(crate) fn record_unstarted_cleanup(
        &mut self,
        value: SessionCleanupObservation,
    ) -> Result<(), ServerFailure> {
        if self.slots.contains_key(&value.session())
            || self
                .evidence
                .get(&value.session())
                .is_some_and(|previous| *previous != value)
        {
            return Err(failure(ServerFailureClass::RecoveryRequired));
        }
        self.evidence.insert(value.session(), value);
        Ok(())
    }
    pub(crate) fn forget_unstarted(&mut self, session: SessionKey) {
        if !self.slots.contains_key(&session) {
            self.evidence.remove(&session);
        }
    }
    pub fn record_cleanup(
        &mut self,
        value: SessionCleanupObservation,
    ) -> Result<(), ServerFailure> {
        if let Some(existing) = self.evidence.get(&value.session()) {
            if *existing != value {
                return Err(failure(ServerFailureClass::RecoveryRequired));
            }
            return Ok(());
        }
        let slot = self
            .slots
            .get(&value.session())
            .ok_or_else(|| failure(ServerFailureClass::CleanupIncomplete))?;
        if slot
            .first_reason
            .is_some_and(|reason| reason != value.first_reason())
        {
            return Err(failure(ServerFailureClass::CleanupIncomplete));
        }
        if let Some(terminal) = slot.observations.borrow().lifecycle.terminal_result()
            && terminal.retirement_reason() != value.first_reason()
        {
            return Err(failure(ServerFailureClass::CleanupIncomplete));
        }
        self.evidence.insert(value.session(), value);
        Ok(())
    }
    pub fn cleanup_for(
        &self,
        request: RetirementRequest<'_>,
    ) -> Result<Option<Vec<SessionCleanupObservation>>, ServerFailure> {
        let mut result = Vec::new();
        for session in request.sessions() {
            let Some(value) = self.evidence.get(&session.session()).copied() else {
                return Ok(None);
            };
            if value.first_reason() != session.first_reason()
                || !matches!(value.result(), CleanupResult::Confirmed(_))
            {
                return Err(failure(ServerFailureClass::CleanupIncomplete));
            }
            result.push(value);
        }
        Ok(Some(result))
    }
    pub async fn close(mut self) -> Result<(), ServerFailure> {
        let slots = std::mem::take(&mut self.slots);
        let mut failure_result = None;
        for (_, slot) in slots {
            drop(slot.sender);
            drop(slot.subscriptions);
            if let Err(value) = slot
                .join
                .await
                .map_err(|_| failure(ServerFailureClass::CleanupIncomplete))
                .and_then(|v| v)
            {
                failure_result = Some(value);
            }
        }
        for task in self.abandoned {
            if let Err(error) = task
                .await
                .map_err(|_| failure(ServerFailureClass::CleanupIncomplete))
                .and_then(|value| value)
            {
                failure_result = Some(error);
            }
        }
        failure_result.map_or(Ok(()), Err)
    }
}
/// A foreground native absence proof can be inconclusive while the terminal's
/// retained cleanup service still owns the original PTY and retries that proof.
/// Keep the owner through a bounded retry window, without changing its terminal
/// result or turning a timeout into a positive cleanup receipt.
async fn await_cleanup_reproof(
    owner: &dyn RuntimeOwnerPort,
    first: SessionCleanupObservation,
) -> Result<SessionCleanupObservation, ServerFailure> {
    if matches!(first.result(), CleanupResult::Confirmed(_)) {
        return Ok(first);
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Some(latest) = owner.cleanup_snapshot() {
            if latest.session() != first.session() || latest.first_reason() != first.first_reason()
            {
                return Err(failure(ServerFailureClass::CleanupIncomplete));
            }
            if matches!(latest.result(), CleanupResult::Confirmed(_)) {
                return Ok(latest);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(first);
        }
        tokio::time::sleep_until(
            deadline.min(tokio::time::Instant::now() + std::time::Duration::from_millis(50)),
        )
        .await;
    }
}

async fn owner_loop(
    mut owner: Box<dyn RuntimeOwnerPort>,
    mut requests: mpsc::Receiver<OwnerRequest>,
    mut subscriptions: mpsc::Receiver<SubscriptionRequest>,
    observations: watch::Sender<RetainedRuntimeObservations>,
    cleanup: Arc<dyn RuntimeCleanupPort>,
) -> Result<(), ServerFailure> {
    let mut first_cleanup = None;
    // A retirement can hold this owner's lane while its native cleanup is
    // re-proved. Other owners must keep refreshing their own retained facts.
    let mut poll = tokio::time::interval(std::time::Duration::from_millis(50));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let request = tokio::select! { biased;
            Some(request) = requests.recv() => Some(request),
            // Disable the timer after both ingress lanes close so it cannot
            // keep an otherwise drained owner alive and prevent its join.
            _ = poll.tick(), if !requests.is_closed() || !subscriptions.is_closed() => {
                observations.send_replace(RetainedRuntimeObservations::capture(owner.as_ref()));
                None
            },
            Some(request) = subscriptions.recv() => {
                let _ = request.reply.send(owner.subscribe(request.fence));
                None
            },
            else => break,
        };
        let Some(request) = request else {
            continue;
        };
        match request {
            OwnerRequest::Authority(transition, reply) => {
                let result = match owner.transition_authority(*transition) {
                    Ok(future) => future.await,
                    Err(error) => Err(error),
                };
                let _ = reply.send(result);
            }
            OwnerRequest::Retire(request, reply) => {
                let result = match first_cleanup {
                    Some(value) => Ok(value),
                    None => owner.retire(request.request()).await,
                };
                let result = match result {
                    Ok(value) => await_cleanup_reproof(owner.as_ref(), value).await,
                    Err(error) => Err(error),
                };
                if let Ok(value) = result {
                    first_cleanup = Some(value);
                }
                let _ = reply.send(result);
            }
            OwnerRequest::Inspect(reply) => {
                observations.send_replace(RetainedRuntimeObservations::capture(owner.as_ref()));
                let _ = reply.send(());
            }
        }
        observations.send_replace(RetainedRuntimeObservations::capture(owner.as_ref()));
    }
    if first_cleanup
        .or(owner.cleanup_snapshot())
        .is_some_and(|v| matches!(v.result(), CleanupResult::Confirmed(_)))
    {
        return Ok(());
    }
    let session = owner.session();
    let result = cleanup.abandon(owner).await?;
    if result.session() == session && matches!(result.result(), CleanupResult::Confirmed(_)) {
        Ok(())
    } else {
        Err(failure(ServerFailureClass::CleanupIncomplete))
    }
}
