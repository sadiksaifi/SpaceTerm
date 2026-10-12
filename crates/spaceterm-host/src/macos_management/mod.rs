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

#[cfg(target_os = "macos")]
#[path = "authorization.rs"]
pub mod authorization;
#[path = "demand.rs"]
pub mod demand;

/// Reads SMAppService for the exact protected service-owner bundle. This is
/// read-only status, not native authorization, installation or server readiness.
/// The caller must retain the admitted bundle and its fixed plist through use.
#[cfg(target_os = "macos")]
pub fn observe_background_items(
    key: crate::startup::AccountBuildKey,
) -> Result<BackgroundItemsStatus, NativeFailure> {
    crate::native_deployment::contracts::validate_key(key)?;
    type Observe = unsafe extern "C" fn(u32, u32, *mut u32) -> u32;
    // SAFETY: this is the fixed management.h function ABI in the admitted native closure.
    let observe: Observe = unsafe {
        crate::native_management::facade::native_api::linked_function(
            c"st_management_observe_background_items",
        )?
    };
    let build = match key.build_identity {
        crate::startup::BuildIdentity::Release => 1,
        crate::startup::BuildIdentity::Preflight => 2,
        crate::startup::BuildIdentity::Development => 3,
    };
    let mut status = u32::MAX;
    // SAFETY: the native adapter writes one bounded scalar and retains no Rust pointer.
    let result = unsafe { observe(build, key.account_uid, &mut status) };
    if result != 0 {
        return Err(NativeFailure::try_from(result)?);
    }
    let status = BackgroundItemsStatus::from_native(status);
    if status == BackgroundItemsStatus::Unavailable {
        return Err(NativeFailure::NativeUnavailable);
    }
    Ok(status)
}
