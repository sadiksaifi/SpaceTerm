//! Engine-free runtime authority comparisons and final-effect barrier replies.
//! Constructors validate shape only. The serialized actor installs authority;
//! the worker acknowledges only after its final-effect lock and transition cleanup.

use crate::authority::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeAuthorityParts {
    pub session: SessionKey,
    pub grant: Option<ControlGrant>,
    pub connection: Option<ConnectionBinding>,
    pub context: ContextGeneration,
    pub focused_pane: bool,
    pub terminal_input_focus: bool,
    pub native_effects_eligible: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeAuthority(RuntimeAuthorityParts);
impl RuntimeAuthority {
    pub fn new(parts: RuntimeAuthorityParts) -> Result<Self, FenceError> {
        if parts.session.epoch.get() == 0
            || parts.session.generation.get() == 0
            || parts.grant.is_some_and(|grant| {
                grant.pane() != parts.session.pane || grant.generation().get() == 0
            })
            || parts.connection.is_some_and(|connection| {
                parts
                    .grant
                    .is_none_or(|grant| grant.client() != connection.client)
                    || connection.generation.get() == 0
            })
            || (parts.focused_pane && parts.connection.is_none())
            || (parts.terminal_input_focus && !parts.focused_pane)
            || (parts.native_effects_eligible && !parts.terminal_input_focus)
        {
            return Err(FenceError::WrongBinding);
        }
        Ok(Self(parts))
    }
    pub const fn parts(self) -> RuntimeAuthorityParts {
        self.0
    }
    pub const fn session(self) -> SessionKey {
        self.0.session
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeAuthorityTransition {
    expected: RuntimeAuthority,
    next: RuntimeAuthority,
}
impl RuntimeAuthorityTransition {
    pub fn new(expected: RuntimeAuthority, next: RuntimeAuthority) -> Result<Self, FenceError> {
        if expected.session() != next.session() {
            return Err(FenceError::WrongBinding);
        }
        Ok(Self { expected, next })
    }
    pub const fn expected(self) -> RuntimeAuthority {
        self.expected
    }
    pub const fn next(self) -> RuntimeAuthority {
        self.next
    }
}
/// Comparison acknowledgement, not native authority or cleanup proof.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthorityApplied(RuntimeAuthorityTransition);
impl AuthorityApplied {
    pub const fn new(transition: RuntimeAuthorityTransition) -> Self {
        Self(transition)
    }
    pub const fn transition(self) -> RuntimeAuthorityTransition {
        self.0
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EffectDisposition {
    Applied,
    StaleAuthority,
    Cancelled,
    Retired,
}
/// The terminal outcome is independent of process and resource cleanup.
/// Explicit retirement retains the first accepted reason, not a later EOF.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeTerminalStatus {
    OrdinaryExit {
        exit_status: Option<i32>,
    },
    StartupFailed {
        class: crate::organization::FailureClass,
    },
    RuntimeFailed {
        class: crate::organization::FailureClass,
    },
    Retired {
        reason: crate::organization::RetirementReason,
    },
}

/// Exact Session result retained even without presentation subscribers.
/// Construction checks comparison shape, not process cleanup or execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionTerminalResult {
    session: SessionKey,
    status: RuntimeTerminalStatus,
}
impl SessionTerminalResult {
    pub fn new(session: SessionKey, status: RuntimeTerminalStatus) -> Result<Self, FenceError> {
        use crate::organization::RetirementReason;
        if session.epoch.get() == 0
            || session.generation.get() == 0
            || matches!(
                status,
                RuntimeTerminalStatus::Retired {
                    reason: RetirementReason::OrdinaryExit | RetirementReason::RuntimeFailure
                }
            )
        {
            return Err(FenceError::WrongBinding);
        }
        Ok(Self { session, status })
    }
    pub const fn session(self) -> SessionKey {
        self.session
    }
    pub const fn status(self) -> RuntimeTerminalStatus {
        self.status
    }
    pub const fn retirement_reason(self) -> crate::organization::RetirementReason {
        use crate::organization::RetirementReason;
        match self.status {
            RuntimeTerminalStatus::OrdinaryExit { .. } => RetirementReason::OrdinaryExit,
            RuntimeTerminalStatus::StartupFailed { .. }
            | RuntimeTerminalStatus::RuntimeFailed { .. } => RetirementReason::RuntimeFailure,
            RuntimeTerminalStatus::Retired { reason } => reason,
        }
    }
}

/// Retained terminal result plus independently classified cleanup disposition.
/// A successful cleanup never changes a failed terminal into a successful exit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeCompletion {
    terminal: SessionTerminalResult,
    cleanup: crate::retirement::SessionCleanupObservation,
}
impl RuntimeCompletion {
    pub fn new(
        terminal: SessionTerminalResult,
        cleanup: crate::retirement::SessionCleanupObservation,
    ) -> Result<Self, FenceError> {
        if terminal.session() != cleanup.session()
            || terminal.retirement_reason() != cleanup.first_reason()
        {
            return Err(FenceError::WrongBinding);
        }
        Ok(Self { terminal, cleanup })
    }
    pub const fn terminal(self) -> SessionTerminalResult {
        self.terminal
    }
    pub const fn cleanup(self) -> crate::retirement::SessionCleanupObservation {
        self.cleanup
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeLifecycle {
    Starting,
    Running,
    Retiring { terminal: SessionTerminalResult },
    Finished(RuntimeCompletion),
}
impl RuntimeLifecycle {
    pub const fn terminal_result(self) -> Option<SessionTerminalResult> {
        match self {
            Self::Retiring { terminal } => Some(terminal),
            Self::Finished(completion) => Some(completion.terminal()),
            Self::Starting | Self::Running => None,
        }
    }
}
