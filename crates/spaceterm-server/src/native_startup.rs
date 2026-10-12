//! Safe native receiver adapter. Ordinary server effects require a completed
//! receiver, exact retriever retirement and current native startup authority.

use spaceterm_host::{
    native_deployment::contracts::NativeFailure,
    native_receiver::{
        AdmittedNativeStartup, NativeReceiver, ReceiverAuthority, ReceiverProtection,
        SystemReceiverProtection,
    },
};

/// Completes native admission before runtime construction or listener creation.
/// The host owns native calls, memory protection, descriptors and child cleanup.
pub fn admit(
    receiver: NativeReceiver,
    authority: &dyn ReceiverAuthority,
) -> Result<AdmittedNativeStartup, NativeFailure> {
    receiver.complete(&SystemReceiverProtection, authority)
}

/// Composition boundary for injected native peers. The server remains safe Rust;
/// tests substitute kernel observations, not STN2, memory or child retirement.
pub fn admit_using(
    receiver: NativeReceiver,
    protection: &dyn ReceiverProtection,
    authority: &dyn ReceiverAuthority,
) -> Result<AdmittedNativeStartup, NativeFailure> {
    receiver.complete(protection, authority)
}
