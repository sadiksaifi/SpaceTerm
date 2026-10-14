//! Unavailable Linux custody contracts retained by the native startup ports.

use std::convert::Infallible;

/// Opaque custody required by the deferred native startup ports.
/// No production issuer exists, so this value cannot be constructed.
#[expect(
    dead_code,
    reason = "the deferred native custody issuer is unavailable"
)]
pub(crate) struct NativeLinuxCustody {
    _unavailable: Infallible,
}
