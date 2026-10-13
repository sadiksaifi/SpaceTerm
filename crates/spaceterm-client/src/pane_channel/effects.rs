use super::*;
use spaceterm_core::native_requests::{PasteApprovalId, PasteDecision};
use spaceterm_terminal::InputModifiers;
use std::num::NonZeroU64;
use tokio::time::{Duration, Instant};

/// Canonical commands remain unchanged. Copy needs a separate port because an
/// ordinary Key cannot enforce enhanced-only encoding at the owning runtime.
#[derive(Clone, Debug, PartialEq)]
#[expect(
    clippy::large_enum_variant,
    reason = "Consumed synchronously; boxing would allocate for every input effect"
)]
pub enum PaneRuntimeEffect {
    Command(RuntimeEffect),
    /// Forward the Copy chord only if enhanced keyboard reporting is active at
    /// the final runtime effect. Never emit legacy text, including Ctrl+C.
    Copy(InputModifiers),
}
impl From<RuntimeEffect> for PaneRuntimeEffect {
    fn from(command: RuntimeEffect) -> Self {
        Self::Command(command)
    }
}
/// RuntimeCommand.request / RuntimeOutcome.request plus the original exact fence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EffectCorrelation {
    request: NonZeroU64,
    fence: EffectFence,
}
impl EffectCorrelation {
    pub fn new(request: u64, fence: EffectFence) -> Result<Self, ClientFailure> {
        Ok(Self {
            request: NonZeroU64::new(request).ok_or_else(invalid_peer)?,
            fence,
        })
    }
    pub fn request(self) -> u64 {
        self.request.get()
    }
    pub fn fence(self) -> EffectFence {
        self.fence
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectAdmission {
    /// Legacy ports cannot report completion. This never means the PTY wrote.
    Queued,
    Correlated(EffectCorrelation),
}
impl EffectAdmission {
    pub fn correlation(self) -> Option<EffectCorrelation> {
        match self {
            Self::Queued => None,
            Self::Correlated(value) => Some(value),
        }
    }
}
#[derive(Clone, Copy)]
pub(super) enum PendingEffectKind {
    PasteWrite,
    PasteResolve {
        approval: PasteApprovalId,
        decision: PasteDecision,
    },
}
pub(super) struct PendingEffect {
    pub correlation: EffectCorrelation,
    pub kind: PendingEffectKind,
    pub expires: Instant,
    pub _slot: tokio::sync::OwnedSemaphorePermit,
}
impl PaneChannel {
    pub(super) fn effect_slot(&self) -> Result<tokio::sync::OwnedSemaphorePermit, ClientFailure> {
        self.inner
            .state
            .lock()
            .map_err(|_| stale())?
            .effects
            .retain(|_, pending| Instant::now() < pending.expires);
        Arc::clone(&self.inner.effect_slots)
            .try_acquire_owned()
            .map_err(|_| resource_limit())
    }
    pub(super) fn track_effect(
        &self,
        permit: &EffectPermit,
        admission: EffectAdmission,
        pending: Option<(PendingEffectKind, tokio::sync::OwnedSemaphorePermit)>,
    ) -> Result<EffectAdmission, ClientFailure> {
        let Some(correlation) = admission.correlation() else {
            return Ok(admission);
        };
        if correlation.fence != permit.fence || !permit.is_current() {
            return Err(stale());
        }
        let mut state = self.inner.state.lock().map_err(|_| stale())?;
        // Reconnect resets this connection-local sequence under the same lock.
        // An old admission must not advance the replacement connection's high-water.
        let current = current_fence(
            &state.facts,
            self.inner.pane,
            permit.fence.content().is_some(),
        )?;
        if !permit.fence.is_current(&current) {
            return Err(stale());
        }
        if correlation.request() <= state.effect_high_water {
            return Err(invalid_peer());
        }
        state.effect_high_water = correlation.request();
        if let Some((kind, slot)) = pending {
            let expires = Instant::now()
                .checked_add(Duration::from_millis(
                    spaceterm_core::native_requests::PASTE_APPROVAL_DEADLINE_MILLIS,
                ))
                .ok_or_else(exhausted)?;
            state.effects.insert(
                correlation.request(),
                PendingEffect {
                    correlation,
                    kind,
                    expires,
                    _slot: slot,
                },
            );
        }
        Ok(admission)
    }
    /// The authenticated adapter maps the existing RuntimeOutcome request, fence
    /// and classified failure. Queue admission is not write completion evidence.
    pub fn receive_runtime_outcome(
        &self,
        correlation: EffectCorrelation,
        result: Result<(), ClientFailure>,
    ) -> Result<(), ClientFailure> {
        // Serialize reply delivery with admission so a fast worker cannot
        // publish completion before its correlation is retained.
        let _admission = self.inner.effect_admission.lock().map_err(|_| stale())?;
        self.receive_runtime_outcome_inner(correlation, result)
    }
    fn receive_runtime_outcome_inner(
        &self,
        correlation: EffectCorrelation,
        result: Result<(), ClientFailure>,
    ) -> Result<(), ClientFailure> {
        if !self.inner.admitted() || !self.lease.active.load(Ordering::Acquire) {
            return Err(stale());
        }
        let mut state = self.inner.state.lock().map_err(|_| stale())?;
        let current = current_fence(
            &state.facts,
            self.inner.pane,
            correlation.fence.content().is_some(),
        )?;
        let pending = state
            .effects
            .get(&correlation.request())
            .ok_or_else(stale)?;
        if pending.correlation != correlation
            || !correlation.fence.is_current(&current)
            || Instant::now() >= pending.expires
        {
            return Err(stale());
        }
        let event = match pending.kind {
            PendingEffectKind::PasteWrite => PaneEvent::PasteWritten {
                correlation,
                result,
            },
            PendingEffectKind::PasteResolve { approval, decision } => PaneEvent::PasteResolved {
                correlation,
                approval,
                decision,
                result,
            },
        };
        state.effects.remove(&correlation.request());
        events::publish(&mut state, event, true);
        Ok(())
    }
}

fn outcome_result(
    failure: Option<&spaceterm_protocol::wire::common::Failure>,
) -> Result<Result<(), ClientFailure>, ClientFailure> {
    use spaceterm_protocol::wire::common::FailureClass;
    let Some(failure) = failure else {
        return Ok(Ok(()));
    };
    let class = match FailureClass::try_from(failure.classification).map_err(|_| invalid_peer())? {
        FailureClass::FailureUnspecified => return Err(invalid_peer()),
        FailureClass::Invalid | FailureClass::Conflict => ClientFailureClass::InvalidPeer,
        FailureClass::Stale | FailureClass::ResyncRequired => ClientFailureClass::StaleAuthority,
        FailureClass::Denied => ClientFailureClass::AuthorizationDenied,
        FailureClass::NotFound => ClientFailureClass::TargetNotFound,
        FailureClass::Busy | FailureClass::Limit => ClientFailureClass::ResourceLimit,
        FailureClass::Unavailable => ClientFailureClass::Unavailable,
        FailureClass::UncertainDurability => ClientFailureClass::RecoveryRequired,
        FailureClass::Retired => ClientFailureClass::Cancelled,
    };
    Ok(Err(ClientFailure::new(class)))
}
macro_rules! runtime_outcome {
    ($method:ident, $profile:ident) => {
        impl PaneChannel {
            pub fn $method(
                &self,
                outcome: &spaceterm_protocol::wire::$profile::RuntimeOutcome,
            ) -> Result<(), ClientFailure> {
                let _admission = self.inner.effect_admission.lock().map_err(|_| stale())?;
                // The legacy wire fence omits content. Recover that comparison
                // only from the exact retained request, never current content.
                let content = self
                    .inner
                    .state
                    .lock()
                    .map_err(|_| stale())?
                    .effects
                    .get(&outcome.request)
                    .ok_or_else(stale)?
                    .correlation
                    .fence()
                    .content();
                let fence = spaceterm_protocol::convert::identity::decode_fence(
                    outcome.fence.as_ref().ok_or_else(invalid_peer)?,
                    content,
                )?;
                self.receive_runtime_outcome_inner(
                    EffectCorrelation::new(outcome.request, fence)?,
                    outcome_result(outcome.failure.as_ref())?,
                )
            }
        }
    };
}
runtime_outcome!(receive_v1_runtime_outcome, v1);
runtime_outcome!(receive_v2_runtime_outcome, v2);
