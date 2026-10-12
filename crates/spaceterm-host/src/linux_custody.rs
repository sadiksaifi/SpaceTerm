//! Native Linux custody and release leases, owned by native-authority A1 and A3.

use std::convert::Infallible;

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the native custody issuer must consume this witness; production admission remains closed"
    )
)]
mod mount_probe;
pub mod support_policy;

/// Retained native custody. Only A's private OS-anchored composition may issue it.
#[expect(
    dead_code,
    reason = "native-authority A1 replaces the unavailable seed"
)]
pub(crate) struct NativeLinuxCustody {
    _unavailable: Infallible,
}

/// Noncloneable release fence held through credential retrieval and delivery.
#[expect(
    dead_code,
    reason = "native-authority A3 replaces the unavailable seed"
)]
pub(crate) struct LinuxReleaseGuard {
    _unavailable: Infallible,
}
