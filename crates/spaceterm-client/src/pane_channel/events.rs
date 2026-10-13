use super::*;
use spaceterm_core::native_requests::NativeRequestKind;
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
    Native {
        request: NativeRequestLease,
        timing: NativeTiming,
    },
}
pub(super) struct PendingNative {
    pub request: NativeRequestLease,
    pub not_before: Instant,
    pub deadline: Option<Instant>,
}
pub(super) struct Subscription {
    sender: mpsc::Sender<PaneEvent>,
    valid: Arc<AtomicBool>,
}
pub struct PaneEvents {
    receiver: mpsc::Receiver<PaneEvent>,
    valid: Arc<AtomicBool>,
    channel: Weak<ChannelInner>,
}
impl PaneEvents {
    pub async fn next(&mut self) -> Result<Option<PaneEvent>, ClientFailure> {
        loop {
            if !self.valid.load(Ordering::Acquire) {
                return Err(resource_limit());
            }
            let Some(event) = self.receiver.recv().await else {
                return Ok(None);
            };
            if !self.valid.load(Ordering::Acquire) {
                return Err(resource_limit());
            }
            if let PaneEvent::Native { request, .. } = &event {
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
        // At most 56 requests are retained, leaving eight lifecycle slots. A new
        // subscriber recovers pending identities, not completed work or new deadlines.
        for pending in state.native.values() {
            let fence = pending.request.fence();
            if current_fence(&state.facts, self.inner.pane, fence.content().is_some())
                .is_ok_and(|current| fence.is_current(&current))
            {
                sender
                    .try_send(PaneEvent::Native {
                        request: pending.request.clone(),
                        timing: NativeTiming {
                            not_before: pending.not_before.saturating_duration_since(now),
                            expires_after: pending
                                .deadline
                                .map(|deadline| deadline.saturating_duration_since(now)),
                        },
                    })
                    .map_err(|_| resource_limit())?;
            }
        }
        let valid = Arc::new(AtomicBool::new(true));
        state.events = Some(Subscription {
            sender,
            valid: Arc::clone(&valid),
        });
        Ok(PaneEvents {
            receiver,
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
            },
        );
        publish(&mut state, PaneEvent::Native { request, timing }, false);
        Ok(())
    }
    pub fn complete_native(
        &self,
        permit: &EffectPermit,
        completion: NativeCompletion,
    ) -> Result<EffectAdmission, ClientFailure> {
        if permit.kind != EffectKind::NativeCompletion
            || !Weak::ptr_eq(&permit.channel, &Arc::downgrade(&self.inner))
            || !permit.is_current()
        {
            return Err(stale());
        }
        let captured = {
            let mut state = self.inner.state.lock().map_err(|_| stale())?;
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
            let captured = pending.request.0.permit.clone();
            state.native.remove(&completion.id().get());
            captured
        };
        // Final dequeue retains the request's exact fence and original deadline.
        self.inner.shared.peer.try_effect(
            captured,
            FencedRuntimeCommand {
                fence: completion.fence(),
                command: RuntimeEffect::CompleteNativeRequest(completion),
            },
        )
    }
}
