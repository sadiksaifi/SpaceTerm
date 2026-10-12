//! Content-free Background Items observations, separate from server registration.

use crate::native_deployment::contracts::NativeFailure;

/// Typed SMAppService observation. Unknown values stay unavailable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackgroundItemsStatus {
    /// OS registration not currently active.
    NotRegistered,
    /// OS reports service enabled, not authenticated server readiness.
    Enabled,
    /// User approval or Background Items recovery is needed.
    RequiresApproval,
    /// OS could not locate the service owner.
    NotFound,
    /// Native status could not be observed.
    Unavailable,
}
impl BackgroundItemsStatus {
    /// Decode an OS observation without inferring installation or identity.
    pub fn from_native(value: u32) -> Self {
        match value {
            0 => Self::NotRegistered,
            1 => Self::Enabled,
            2 => Self::RequiresApproval,
            3 => Self::NotFound,
            _ => Self::Unavailable,
        }
    }
    /// Classify supervision without ever calling it a missing registration.
    pub fn supervision(self) -> Result<(), NativeFailure> {
        match self {
            Self::Enabled => Ok(()),
            Self::RequiresApproval => Err(NativeFailure::BackgroundItemsSuppressed),
            Self::NotFound | Self::NotRegistered => Err(NativeFailure::SupervisorUnavailable),
            Self::Unavailable => Err(NativeFailure::NativeUnavailable),
        }
    }
}
