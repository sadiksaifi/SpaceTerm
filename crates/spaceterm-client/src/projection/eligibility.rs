use crate::failure::{ClientFailure, ClientFailureClass};
use spaceterm_core::authority::EffectFence;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
pub(crate) struct Eligibility(Arc<Mutex<State>>);
#[derive(Default)]
struct State {
    admission: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    fence: Option<EffectFence>,
    frozen: bool,
    native: bool,
    presentation: bool,
}
impl Eligibility {
    pub(crate) fn bind_admission(&self, gate: Arc<dyn Fn() -> bool + Send + Sync>) {
        if let Ok(mut state) = self.0.lock() {
            state.admission = Some(gate);
        }
    }
    pub(crate) fn fresh(&self) -> Result<Self, ClientFailure> {
        let state = self.0.lock().map_err(|_| stale())?;
        Ok(Self(Arc::new(Mutex::new(State {
            admission: state.admission.clone(),
            ..State::default()
        }))))
    }
    pub(crate) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub(crate) fn online(&self, fence: EffectFence, presentation: bool, native: bool) {
        if let Ok(mut state) = self.0.lock() {
            state.fence = Some(fence);
            state.frozen = false;
            state.native = native;
            state.presentation = presentation;
        }
    }
    pub(crate) fn disable_native(&self) {
        if let Ok(mut state) = self.0.lock() {
            state.native = false;
        }
    }
    pub(crate) fn freeze(&self) {
        if let Ok(mut state) = self.0.lock() {
            state.frozen = true;
        }
    }
    pub(crate) fn revoke(&self) {
        if let Ok(mut state) = self.0.lock() {
            state.fence = None;
            state.frozen = false;
        }
    }
    pub(crate) fn fence(&self) -> Option<EffectFence> {
        self.0.lock().ok().and_then(|state| state.fence)
    }
    pub(crate) fn allows_presentation(&self, fence: EffectFence) -> bool {
        self.0.lock().is_ok_and(|state| {
            state.admission.as_ref().is_none_or(|allows| allows())
                && !state.frozen
                && state.presentation
                && state
                    .fence
                    .is_some_and(|current| fence.is_current(&current))
        })
    }
    pub(crate) fn current(&self) -> Result<EffectFence, ClientFailure> {
        let state = self.0.lock().map_err(|_| stale())?;
        state
            .fence
            .filter(|_| {
                state.admission.as_ref().is_none_or(|allows| allows())
                    && !state.frozen
                    && state.native
            })
            .ok_or_else(stale)
    }
    pub(crate) fn allows(&self, fence: EffectFence, paint: bool) -> bool {
        self.0.lock().is_ok_and(|state| {
            state.admission.as_ref().is_none_or(|allows| allows())
                && (paint || (!state.frozen && state.native))
                && state
                    .fence
                    .is_some_and(|current| fence.is_current(&current))
        })
    }
    pub(crate) fn frozen(&self) -> bool {
        self.0.lock().is_ok_and(|state| state.frozen)
    }
}
pub(crate) fn stale() -> ClientFailure {
    ClientFailure::new(ClientFailureClass::StaleAuthority)
}
