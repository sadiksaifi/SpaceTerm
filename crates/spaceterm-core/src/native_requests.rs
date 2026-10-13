//! Bounded native-effect requests. Serialization never grants native authority.
//!
//! The runtime captures the original eligible fence when an output sequence
//! begins. Server routing/completion and client execution revalidate independently.
//! NativeText is transient and must never enter journals, operation outcomes,
//! diagnostics or persisted client view state.

use std::fmt;
use std::num::NonZeroU64;

use crate::authority::EffectFence;
use crate::observations::MonotonicTick;

pub const MAX_NATIVE_TEXT_BYTES: usize = 1024 * 1024;
pub const OSC52_READ_DEADLINE_MILLIS: u64 = 1_000;
pub const PASTE_APPROVAL_DEADLINE_MILLIS: u64 = 30_000;

/// Closed, content-free contract failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum NativeRequestError {
    #[error("native request identity or completion is inconsistent")]
    InvalidBinding,
    #[error("native request text exceeds its bound")]
    Limit,
    #[error("native request deadline is invalid")]
    InvalidDeadline,
    #[error("native request counter exhausted")]
    Exhausted,
}

macro_rules! native_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub struct $name(NonZeroU64);
        impl $name {
            pub fn new(value: u64) -> Result<Self, NativeRequestError> {
                NonZeroU64::new(value)
                    .map(Self)
                    .ok_or(NativeRequestError::InvalidBinding)
            }
            pub const fn get(self) -> u64 {
                self.0.get()
            }
            pub fn checked_successor(self) -> Result<Self, NativeRequestError> {
                Self::new(
                    self.get()
                        .checked_add(1)
                        .ok_or(NativeRequestError::Exhausted)?,
                )
            }
        }
    };
}
native_id!(
    NativeRequestId,
    "Owner-issued request identity, unique within its Session."
);
native_id!(
    PasteApprovalId,
    "Identity of one immutable sanitized paste approval, bound to the original fence."
);

/// Bounded UTF-8 text; empty is valid for clipboard clearing and denied read replies.
/// Paste sanitation and unsafe-paste classification remain portable runtime policy.
#[derive(Clone, Eq, PartialEq)]
pub struct NativeText(String);
impl NativeText {
    pub fn new(value: String) -> Result<Self, NativeRequestError> {
        if value.len() > MAX_NATIVE_TEXT_BYTES {
            return Err(NativeRequestError::Limit);
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn into_string(self) -> String {
        self.0
    }
}
impl fmt::Debug for NativeText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeText([redacted])")
    }
}

/// The two requests supported by the existing OSC 7701 filter. These are notices
/// offering person-initiated setup, never claims that an OS grant was obtained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PermissionNotice {
    screen_recording: bool,
    accessibility: bool,
}
impl PermissionNotice {
    pub fn new(screen_recording: bool, accessibility: bool) -> Result<Self, NativeRequestError> {
        if !(screen_recording || accessibility) {
            return Err(NativeRequestError::InvalidBinding);
        }
        Ok(Self {
            screen_recording,
            accessibility,
        })
    }
    pub const fn screen_recording(self) -> bool {
        self.screen_recording
    }
    pub const fn accessibility(self) -> bool {
        self.accessibility
    }
}

/// Content-free terminal attention facts. Native notification text is composed locally.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotificationNotice {
    Bell,
    CommandFinished {
        exit_status: Option<i32>,
        duration_millis: u64,
    },
}

/// No executable, server path, clipboard proof or operating-system permission is represented.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeRequestKind {
    ClipboardRead,
    ClipboardWrite(NativeText),
    UnsafePaste {
        approval: PasteApprovalId,
        text: NativeText,
    },
    Permission(PermissionNotice),
    Notification(NotificationNotice),
}

/// Deadline ticks use the server's epoch clock, not the client's clock. The wire
/// adapter conveys a bounded remaining duration for client-local expiry while
/// the runtime retains this original deadline; network delivery never extends it.
/// Permission notices may have no expiry but require a positive response delay.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeRequest {
    fence: EffectFence,
    id: NativeRequestId,
    issued_at: MonotonicTick,
    not_before: MonotonicTick,
    deadline: Option<MonotonicTick>,
    kind: NativeRequestKind,
}
impl NativeRequest {
    pub fn new(
        fence: EffectFence,
        id: NativeRequestId,
        issued_at: MonotonicTick,
        not_before: MonotonicTick,
        deadline: Option<MonotonicTick>,
        kind: NativeRequestKind,
    ) -> Result<Self, NativeRequestError> {
        if not_before < issued_at || deadline.is_some_and(|end| end <= not_before) {
            return Err(NativeRequestError::InvalidDeadline);
        }
        match &kind {
            NativeRequestKind::ClipboardRead => {
                let elapsed = deadline
                    .and_then(|end| end.checked_elapsed_since(issued_at))
                    .ok_or(NativeRequestError::InvalidDeadline)?;
                if elapsed > OSC52_READ_DEADLINE_MILLIS {
                    return Err(NativeRequestError::InvalidDeadline);
                }
            }
            NativeRequestKind::UnsafePaste { .. } => {
                let elapsed = deadline
                    .and_then(|end| end.checked_elapsed_since(issued_at))
                    .ok_or(NativeRequestError::InvalidDeadline)?;
                if elapsed > PASTE_APPROVAL_DEADLINE_MILLIS {
                    return Err(NativeRequestError::InvalidDeadline);
                }
            }
            NativeRequestKind::ClipboardWrite(_) | NativeRequestKind::Notification(_) => {
                if deadline.is_none() {
                    return Err(NativeRequestError::InvalidDeadline);
                }
            }
            NativeRequestKind::Permission(_) => {
                if not_before <= issued_at {
                    return Err(NativeRequestError::InvalidDeadline);
                }
            }
        }
        Ok(Self {
            fence,
            id,
            issued_at,
            not_before,
            deadline,
            kind,
        })
    }
    pub const fn fence(&self) -> EffectFence {
        self.fence
    }
    pub const fn id(&self) -> NativeRequestId {
        self.id
    }
    pub const fn issued_at(&self) -> MonotonicTick {
        self.issued_at
    }
    pub const fn not_before(&self) -> MonotonicTick {
        self.not_before
    }
    pub const fn deadline(&self) -> Option<MonotonicTick> {
        self.deadline
    }
    pub fn kind(&self) -> &NativeRequestKind {
        &self.kind
    }
}

/// Deliberate decision for the exact pending payload; wrong IDs consume nothing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PasteDecision {
    Confirm,
    Cancel,
}

/// PermissionAcknowledged never means permission granted. ClipboardText is valid
/// only for an explicitly authorized clipboard read, not another native action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeCompletionKind {
    Denied,
    Expired,
    Cancelled,
    ClipboardText(NativeText),
    ClipboardWritten,
    Paste {
        approval: PasteApprovalId,
        decision: PasteDecision,
    },
    PermissionAcknowledged,
    NotificationAcknowledged,
}

/// Shape-checked completion. Its captured fence is not retagged with new authority.
/// The runtime still checks current request, deadline and eligibility before effects.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeCompletion {
    fence: EffectFence,
    id: NativeRequestId,
    kind: NativeCompletionKind,
}
impl NativeCompletion {
    pub fn new(
        request: &NativeRequest,
        kind: NativeCompletionKind,
    ) -> Result<Self, NativeRequestError> {
        let valid = match (&kind, request.kind()) {
            (
                NativeCompletionKind::Denied
                | NativeCompletionKind::Expired
                | NativeCompletionKind::Cancelled,
                _,
            ) => true,
            (NativeCompletionKind::ClipboardText(_), NativeRequestKind::ClipboardRead) => true,
            (NativeCompletionKind::ClipboardWritten, NativeRequestKind::ClipboardWrite(_)) => true,
            (
                NativeCompletionKind::Paste { approval, .. },
                NativeRequestKind::UnsafePaste {
                    approval: expected, ..
                },
            ) => approval == expected,
            (NativeCompletionKind::PermissionAcknowledged, NativeRequestKind::Permission(_)) => {
                true
            }
            (
                NativeCompletionKind::NotificationAcknowledged,
                NativeRequestKind::Notification(_),
            ) => true,
            _ => false,
        };
        if !valid {
            return Err(NativeRequestError::InvalidBinding);
        }
        Ok(Self {
            fence: request.fence(),
            id: request.id(),
            kind,
        })
    }
    pub const fn fence(&self) -> EffectFence {
        self.fence
    }
    pub const fn id(&self) -> NativeRequestId {
        self.id
    }
    pub fn kind(&self) -> &NativeCompletionKind {
        &self.kind
    }
}
