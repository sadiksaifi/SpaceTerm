use super::*;
use spaceterm_core::native_requests::{
    NativeCompletionKind, NativeRequestKind, PasteApprovalId, PasteDecision,
};
use std::time::Duration;
use tokio::{sync::mpsc, time::Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeTiming {
    pub not_before: Duration,
    pub expires_after: Option<Duration>,
}
/// Immutable request residency follows subscribers and retained native work.
/// The adapter must still recheck its EffectPermit before any local effect.
#[derive(Clone)]
pub struct NativeRequestLease(Arc<ChargedRequest>);
struct ChargedRequest {
    request: NativeRequest,
    permit: EffectPermit,
    _charge: crate::projection::budget::Charge,
}
impl NativeRequestLease {
    /// Recheck immediately before the adapter reads or writes native state.
    pub fn is_current(&self) -> bool {
        if !self.0.permit.is_current() {
            return false;
        }
        let Some(channel) = self.0.permit.channel.upgrade() else {
            return false;
        };
        channel.state.lock().is_ok_and(|state| {
            state.native.get(&self.id().get()).is_some_and(|pending| {
                *pending.request == self.0.request && Instant::now() >= pending.not_before
            })
        })
    }
}
#[derive(Clone, Copy)]
pub(super) struct NativeGuard {
    deadline: Option<Instant>,
    clipboard_read: bool,
}
impl NativeGuard {
    pub(super) fn allows(self, channel: &ChannelInner) -> bool {
        self.deadline
            .is_none_or(|deadline| Instant::now() < deadline)
            && (!self.clipboard_read || clipboard_input_visible(channel))
    }
}
fn clipboard_input_visible(channel: &ChannelInner) -> bool {
    channel
        .projection
        .lock()
        .ok()
        .and_then(|projection| projection.current_frame())
        .is_some_and(|frame| {
            frame.permits_native_content_use()
                && frame.frame().is_some_and(|data| {
                    !data
                        .pages()
                        .iter()
                        .any(|page| page.page().parts().modes.hidden_input)
                })
        })
}
impl std::ops::Deref for NativeRequestLease {
    type Target = NativeRequest;
    fn deref(&self) -> &NativeRequest {
        &self.0.request
    }
}
impl std::fmt::Debug for NativeRequestLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.request.fmt(f)
    }
}
#[derive(Clone, Debug)]
pub enum PaneEvent {
    Invalidated,
    PasteWritten {
        correlation: EffectCorrelation,
        result: Result<(), ClientFailure>,
    },
    PasteApproval {
        correlation: EffectCorrelation,
        request: NativeRequestLease,
        timing: NativeTiming,
    },
    PasteResolved {
        correlation: EffectCorrelation,
        approval: PasteApprovalId,
        decision: PasteDecision,
        result: Result<(), ClientFailure>,
    },
    Metadata {
        fence: EffectFence,
        snapshot: Arc<RuntimeMetadataSnapshot>,
    },
    Native {
        request: NativeRequestLease,
        timing: NativeTiming,
    },
}
pub(super) struct PendingNative {
    pub request: NativeRequestLease,
    pub not_before: Instant,
    pub deadline: Option<Instant>,
    pub correlation: Option<EffectCorrelation>,
}
pub(super) struct Subscription {
    sender: mpsc::Sender<PaneEvent>,
    metadata: watch::Sender<Option<Arc<RuntimeMetadataSnapshot>>>,
    valid: Arc<AtomicBool>,
}
pub struct PaneEvents {
    receiver: mpsc::Receiver<PaneEvent>,
    metadata: watch::Receiver<Option<Arc<RuntimeMetadataSnapshot>>>,
    valid: Arc<AtomicBool>,
    channel: Weak<ChannelInner>,
}
impl PaneEvents {
    pub async fn next(&mut self) -> Result<Option<PaneEvent>, ClientFailure> {
        loop {
            if !self.valid.load(Ordering::Acquire) {
                return Err(resource_limit());
            }
            let event = tokio::select! {
                biased;
                event = self.receiver.recv() => {
                    let Some(event) = event else { return Ok(None); };
                    event
                }
                changed = self.metadata.changed() => {
                    if changed.is_err() { return Ok(None); }
                    let Some(snapshot) = self.metadata.borrow_and_update().clone() else { continue; };
                    let Some(channel) = self.channel.upgrade() else { return Ok(None); };
                    if !channel.admitted() { continue; }
                    let state = channel.state.lock().map_err(|_| stale())?;
                    if !state.metadata.as_ref().is_some_and(|current| Arc::ptr_eq(current, &snapshot))
                        || !matches!(state.facts.readiness, PaneReadiness::Live | PaneReadiness::Reconciling)
                        || !matches!(state.facts.runtime.map(|runtime| runtime.phase), Some(SessionPhase::Running { .. }))
                        || channel.lease.upgrade().is_none_or(|lease| !lease.active.load(Ordering::Acquire))
                    {
                        continue;
                    }
                    // Source ordering belongs to the retained Session snapshot.
                    // Presentation uses today's binding/context, without granting effects.
                    let Ok(fence) = bound_fence(&state.facts, channel.pane, false) else { continue; };
                    if snapshot.session() != fence.session() { continue; }
                    PaneEvent::Metadata { fence, snapshot }
                }
            };
            if !self.valid.load(Ordering::Acquire) {
                return Err(resource_limit());
            }
            if let PaneEvent::PasteWritten { correlation, .. }
            | PaneEvent::PasteResolved { correlation, .. } = &event
            {
                let Some(channel) = self.channel.upgrade() else {
                    return Ok(None);
                };
                if !channel.admitted()
                    || !channel.state.lock().is_ok_and(|state| {
                        current_fence(
                            &state.facts,
                            channel.pane,
                            correlation.fence().content().is_some(),
                        )
                        .is_ok_and(|current| correlation.fence().is_current(&current))
                    })
                {
                    continue;
                }
            }
            if let PaneEvent::Native { request, .. } | PaneEvent::PasteApproval { request, .. } =
                &event
            {
                if !request.0.permit.is_current() {
                    continue;
                }
                let Some(channel) = self.channel.upgrade() else {
                    return Ok(None);
                };
                if !channel.state.lock().is_ok_and(|state| {
                    state
                        .native
                        .get(&request.id().get())
                        .is_some_and(|pending| {
                            *pending.request == **request
                                && pending
                                    .deadline
                                    .is_none_or(|deadline| Instant::now() < deadline)
                        })
                }) {
                    continue;
                }
            }
            return Ok(Some(event));
        }
    }
}
pub(super) fn refresh_metadata(state: &mut ChannelState) {
    let Some(subscription) = &state.events else {
        return;
    };
    if subscription.sender.is_closed() {
        state.events = None;
    } else {
        subscription.metadata.send_replace(state.metadata.clone());
    }
}
pub(super) fn publish(state: &mut ChannelState, event: PaneEvent, lifecycle: bool) {
    let Some(subscription) = &state.events else {
        return;
    };
    if subscription.sender.is_closed() {
        state.events = None;
        return;
    }
    if (!lifecycle && subscription.sender.capacity() <= 8)
        || subscription.sender.try_send(event).is_err()
    {
        subscription.valid.store(false, Ordering::Release);
        state.events = None;
    }
}
impl PaneChannel {
    pub fn events(&self) -> Result<PaneEvents, ClientFailure> {
        let mut state = self.inner.state.lock().map_err(|_| stale())?;
        if state
            .events
            .as_ref()
            .is_some_and(|subscription| !subscription.sender.is_closed())
        {
            return Err(resource_limit());
        }
        let now = Instant::now();
        state
            .native
            .retain(|_, pending| pending.deadline.is_none_or(|deadline| now < deadline));
        let (sender, receiver) = mpsc::channel(64);
        // Metadata uses one coalescing slot, not the native/lifecycle queue.
        let (metadata_sender, mut metadata) = watch::channel(state.metadata.clone());
        if state.metadata.is_some() {
            metadata.mark_changed();
        }
        // At most 56 requests are retained, leaving eight lifecycle slots. A new
        // subscriber recovers pending identities, not completed work or new deadlines.
        for pending in state.native.values() {
            let fence = pending.request.fence();
            if current_fence(&state.facts, self.inner.pane, fence.content().is_some())
                .is_ok_and(|current| fence.is_current(&current))
            {
                sender
                    .try_send(native_event(
                        pending.request.clone(),
                        NativeTiming {
                            not_before: pending.not_before.saturating_duration_since(now),
                            expires_after: pending
                                .deadline
                                .map(|deadline| deadline.saturating_duration_since(now)),
                        },
                        pending.correlation,
                    ))
                    .map_err(|_| resource_limit())?;
            }
        }
        let valid = Arc::new(AtomicBool::new(true));
        state.events = Some(Subscription {
            sender,
            metadata: metadata_sender,
            valid: Arc::clone(&valid),
        });
        Ok(PaneEvents {
            receiver,
            metadata,
            valid,
            channel: Arc::downgrade(&self.inner),
        })
    }
    /// The authenticated adapter supplies remaining server-clock delays. Arrival
    /// never extends the original request's bounded lifetime.
    pub fn receive_native(
        &self,
        request: NativeRequest,
        timing: NativeTiming,
    ) -> Result<(), ClientFailure> {
        self.receive_native_inner(request, timing, None)
    }
    /// The adapter must know the originating runtime request. NativeRequest's
    /// own request identity must never be guessed to be that runtime identity.
    pub fn receive_paste_approval(
        &self,
        correlation: EffectCorrelation,
        request: NativeRequest,
        timing: NativeTiming,
    ) -> Result<(), ClientFailure> {
        self.receive_native_inner(request, timing, Some(correlation))
    }
    fn receive_native_inner(
        &self,
        request: NativeRequest,
        timing: NativeTiming,
        correlation: Option<EffectCorrelation>,
    ) -> Result<(), ClientFailure> {
        let _admission = self.inner.effect_admission.lock().map_err(|_| stale())?;
        let permit = self.permit(EffectKind::NativeCompletion)?;
        let mut state = self.inner.state.lock().map_err(|_| stale())?;
        let current = current_fence(
            &state.facts,
            self.inner.pane,
            request.fence().content().is_some(),
        )?;
        if !request.fence().is_current(&current)
            || !permit.fence.is_current(&current)
            || request.id().get() <= state.native_high_water
        {
            return Err(stale());
        }
        if let Some(correlation) = correlation {
            let pending = state
                .effects
                .get(&correlation.request())
                .ok_or_else(stale)?;
            if !matches!(request.kind(), NativeRequestKind::UnsafePaste { .. })
                || !matches!(pending.kind, effects::PendingEffectKind::PasteWrite)
                || pending.correlation != correlation
                || request.fence() != correlation.fence()
                || Instant::now() >= pending.expires
            {
                return Err(stale());
            }
        }
        let maximum_delay = request
            .not_before()
            .checked_elapsed_since(request.issued_at())
            .ok_or_else(invalid_peer)?;
        let maximum_lifetime = request
            .deadline()
            .map(|deadline| {
                deadline
                    .checked_elapsed_since(request.issued_at())
                    .ok_or_else(invalid_peer)
            })
            .transpose()?;
        if timing.not_before > Duration::from_millis(maximum_delay)
            || timing.expires_after.is_some() != maximum_lifetime.is_some()
            || timing
                .expires_after
                .zip(maximum_lifetime)
                .is_some_and(|(remaining, maximum)| {
                    remaining > Duration::from_millis(maximum) || remaining <= timing.not_before
                })
        {
            return Err(invalid_peer());
        }
        if matches!(request.kind(), NativeRequestKind::ClipboardRead)
            && self
                .current_frame()
                .and_then(|frame| {
                    frame.frame().map(|data| {
                        data.pages()
                            .iter()
                            .any(|page| page.page().parts().modes.hidden_input)
                    })
                })
                .unwrap_or(true)
        {
            return Err(stale());
        }
        let now = Instant::now();
        state
            .native
            .retain(|_, pending| pending.deadline.is_none_or(|deadline| now < deadline));
        if state.native.len() >= 56
            || (matches!(request.kind(), NativeRequestKind::UnsafePaste { .. })
                && state.native.values().any(|pending| {
                    matches!(
                        pending.request.kind(),
                        NativeRequestKind::UnsafePaste { .. }
                    )
                }))
        {
            return Err(resource_limit());
        }
        let not_before = now
            .checked_add(timing.not_before)
            .ok_or_else(invalid_peer)?;
        let deadline = timing
            .expires_after
            .map(|duration| now.checked_add(duration).ok_or_else(invalid_peer))
            .transpose()?;
        let text_bytes = match request.kind() {
            NativeRequestKind::ClipboardWrite(text)
            | NativeRequestKind::UnsafePaste { text, .. } => text.as_str().len(),
            _ => 0,
        };
        let bytes = text_bytes.checked_add(1024).ok_or_else(resource_limit)?;
        let charge = self
            .inner
            .shared
            .native_residency
            .reserve(self.inner.pane.server(), bytes as u64)?;
        let captured = EffectPermit {
            fence: request.fence(),
            native: Some(NativeGuard {
                deadline,
                clipboard_read: matches!(request.kind(), NativeRequestKind::ClipboardRead),
            }),
            ..permit
        };
        let request = NativeRequestLease(Arc::new(ChargedRequest {
            request,
            permit: captured,
            _charge: charge,
        }));
        state.native_high_water = request.id().get();
        state.native.insert(
            request.id().get(),
            PendingNative {
                request: request.clone(),
                not_before,
                deadline,
                correlation,
            },
        );
        if let Some(correlation) = correlation {
            state.effects.remove(&correlation.request());
        }
        publish(
            &mut state,
            native_event(request, timing, correlation),
            false,
        );
        Ok(())
    }
    pub fn complete_native(
        &self,
        permit: &EffectPermit,
        completion: NativeCompletion,
    ) -> Result<EffectAdmission, ClientFailure> {
        self.complete_native_command(permit, completion, RuntimeEffect::CompleteNativeRequest)
    }
    pub(super) fn complete_native_command(
        &self,
        permit: &EffectPermit,
        completion: NativeCompletion,
        command: impl FnOnce(NativeCompletion) -> RuntimeEffect,
    ) -> Result<EffectAdmission, ClientFailure> {
        if permit.kind != EffectKind::NativeCompletion
            || !Weak::ptr_eq(&permit.channel, &Arc::downgrade(&self.inner))
            || !permit.is_current()
        {
            return Err(stale());
        }
        let _admission = self.inner.effect_admission.lock().map_err(|_| stale())?;
        let pending = match completion.kind() {
            NativeCompletionKind::Paste { approval, decision } => Some((
                effects::PendingEffectKind::PasteResolve {
                    approval: *approval,
                    decision: *decision,
                },
                self.effect_slot()?,
            )),
            _ => None,
        };
        let captured = {
            let state = self.inner.state.lock().map_err(|_| stale())?;
            let current = current_fence(
                &state.facts,
                self.inner.pane,
                completion.fence().content().is_some(),
            )?;
            let pending = state.native.get(&completion.id().get()).ok_or_else(stale)?;
            let now = Instant::now();
            if completion.fence() != pending.request.fence()
                || !completion.fence().is_current(&current)
                || now < pending.not_before
                || pending.deadline.is_some_and(|deadline| now >= deadline)
                || pending
                    .request
                    .0
                    .permit
                    .native
                    .is_none_or(|guard| !guard.allows(&self.inner))
                || NativeCompletion::new(&pending.request, completion.kind().clone()).is_err()
            {
                return Err(stale());
            }
            pending.request.0.permit.clone()
        };
        // Final dequeue retains the request's exact fence and original deadline.
        let request_id = completion.id();
        let admission = self.inner.shared.peer.try_effect(
            captured.clone(),
            FencedRuntimeCommand {
                fence: completion.fence(),
                command: command(completion),
            },
        )?;
        self.inner
            .state
            .lock()
            .map_err(|_| stale())?
            .native
            .remove(&request_id.get());
        self.track_effect(&captured, admission, pending)
    }
}

fn native_event(
    request: NativeRequestLease,
    timing: NativeTiming,
    correlation: Option<EffectCorrelation>,
) -> PaneEvent {
    match correlation {
        Some(correlation) => PaneEvent::PasteApproval {
            correlation,
            request,
            timing,
        },
        None => PaneEvent::Native { request, timing },
    }
}
