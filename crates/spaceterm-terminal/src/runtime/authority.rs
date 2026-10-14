//! Final-effect guards and admission policy. Guards never cross blocking waits.
use super::ports::{RuntimeFailure, RuntimeStage};
use crate::runtime_contracts::{RuntimeAuthority, RuntimeCommand};
use spaceterm_core::authority::{ContentRevision, EffectFence};
use std::sync::{RwLock, RwLockReadGuard};

/// Worker-local admission incarnation. An eligibility loss cannot be undone for
/// queued work by restoring the same public fence before the worker drains it.
#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) struct AuthorityStamp {
    pub fence: EffectFence,
    pub version: u64,
}
pub(crate) struct Authority {
    pub current: RuntimeAuthority,
    pub sealed: bool,
    pub version: u64,
}
pub(crate) struct AuthorityCell(pub RwLock<Authority>);
impl AuthorityCell {
    pub fn new(current: RuntimeAuthority) -> Self {
        Self(RwLock::new(Authority {
            current,
            sealed: false,
            version: 0,
        }))
    }
    pub fn read(&self) -> Result<RwLockReadGuard<'_, Authority>, RuntimeFailure> {
        self.0
            .read()
            .map_err(|_| RuntimeFailure::internal(RuntimeStage::Authority))
    }
}
impl Authority {
    pub fn permits(
        &self,
        fence: EffectFence,
        command: Option<&RuntimeCommand>,
        revision: Option<ContentRevision>,
    ) -> bool {
        let parts = self.current.parts();
        if self.sealed
            || parts.session != fence.session()
            || parts.grant != Some(fence.grant())
            || parts.connection != Some(fence.connection())
            || parts.context != fence.context()
            || fence
                .content()
                .is_some_and(|expected| revision != Some(expected))
        {
            return false;
        }
        match command {
            Some(RuntimeCommand::InputMethodCommit(_)) => parts.focused_pane,
            Some(
                RuntimeCommand::Key(_)
                | RuntimeCommand::Copy(_)
                | RuntimeCommand::RequestPaste(_)
                | RuntimeCommand::RequestCorrelatedPaste(..)
                | RuntimeCommand::ResolvePaste(..)
                | RuntimeCommand::Pointer(_)
                | RuntimeCommand::Wheel(_),
            ) => parts.terminal_input_focus,
            Some(RuntimeCommand::CompleteNativeRequest(_)) => parts.native_effects_eligible,
            _ => true,
        }
    }
    pub fn capture(&self, fence: EffectFence) -> AuthorityStamp {
        AuthorityStamp {
            fence,
            version: self.version,
        }
    }
    pub fn permits_stamp(
        &self,
        stamp: AuthorityStamp,
        command: Option<&RuntimeCommand>,
        revision: Option<ContentRevision>,
    ) -> bool {
        stamp.version == self.version && self.permits(stamp.fence, command, revision)
    }
    pub fn permits_read(&self, fence: EffectFence) -> bool {
        // Content is a resync comparison, not a control-authority comparison.
        self.permits(fence, None, fence.content())
    }
    pub fn native_stamp(&self) -> Option<AuthorityStamp> {
        let parts = self.current.parts();
        if self.sealed || !parts.native_effects_eligible {
            return None;
        }
        EffectFence::new(
            parts.session,
            parts.grant?,
            parts.connection?,
            parts.context,
            None,
        )
        .ok()
        .map(|fence| self.capture(fence))
    }
    pub fn permits_native(&self, stamp: AuthorityStamp) -> bool {
        self.native_stamp() == Some(stamp)
    }
}
