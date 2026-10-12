//! Guarded macOS boot-credential boundary.
//!
//! A persisted policy observation is comparison data, not release authority.
//! The retained-registration issuer must supply native binding and permit handles
//! before the C adapter can be used. This base has neither handle producer. The
//! production adapter therefore rejects before touching the System Keychain.

use crate::boot_credentials::{BootCredentialAdapter, CredentialPolicyFacts, LockedSecret32};
use crate::native_deployment::contracts::{
    BootCredentialBinding, CredentialReleasePermit, NativeFailure,
};

/// Boot-credential adapter with no account, item, path, or environment selector.
///
/// Native binding issuance and complete persisted-policy evidence remain required.
/// Construction does not authorize inspection, enrollment, or secret retrieval.
#[derive(Default)]
pub struct MacosBootCredentialAdapter;

impl BootCredentialAdapter for MacosBootCredentialAdapter {
    fn inspect_policy(
        &self,
        binding: &BootCredentialBinding,
    ) -> Result<CredentialPolicyFacts, NativeFailure> {
        let observation = inspect_native_binding(binding)?;
        observation.validate()?;
        // The common facts constructor and native retained-handle issuer must be
        // integrated together. Decoded integers must never create native authority.
        Err(NativeFailure::NativeUnavailable)
    }

    fn retrieve_for_startup(
        &self,
        _permit: &CredentialReleasePermit,
        destination: &mut LockedSecret32,
    ) -> Result<(), NativeFailure> {
        destination.wipe();
        Err(NativeFailure::NativeUnavailable)
    }
}

fn inspect_native_binding(
    _binding: &BootCredentialBinding,
) -> Result<KeychainPolicyObservation, NativeFailure> {
    // No native handle can be derived from comparison facts or a caller assertion.
    Err(NativeFailure::NativeUnavailable)
}

/// In-process C observation ABI. It contains no attributes or credential reference.
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub(crate) struct KeychainPolicyObservation {
    pub(crate) schema: u32,
    pub(crate) policy_class: u32,
    pub(crate) interaction_class: u32,
    pub(crate) account_binding_matches: u32,
    pub(crate) code_binding_matches: u32,
    pub(crate) auxiliary_acl_class: u32,
}

const _: () = {
    assert!(size_of::<KeychainPolicyObservation>() == 24);
    assert!(align_of::<KeychainPolicyObservation>() == 4);
    assert!(std::mem::offset_of!(KeychainPolicyObservation, schema) == 0);
    assert!(std::mem::offset_of!(KeychainPolicyObservation, policy_class) == 4);
    assert!(std::mem::offset_of!(KeychainPolicyObservation, interaction_class) == 8);
    assert!(std::mem::offset_of!(KeychainPolicyObservation, account_binding_matches) == 12);
    assert!(std::mem::offset_of!(KeychainPolicyObservation, code_binding_matches) == 16);
    assert!(std::mem::offset_of!(KeychainPolicyObservation, auxiliary_acl_class) == 20);
};

impl KeychainPolicyObservation {
    pub(crate) fn validate(self) -> Result<(), NativeFailure> {
        // All six values must be exactly recognized. Truthy, future, and absent
        // values are not successful policy inspection. Auxiliary class 1 means
        // absent, not "some auxiliary ACL was ignored".
        if self.schema != 1
            || self.policy_class != 1
            || self.interaction_class != 1
            || self.account_binding_matches != 1
            || self.code_binding_matches != 1
            || self.auxiliary_acl_class != 1
        {
            return Err(NativeFailure::CredentialPolicyRejected);
        }
        Ok(())
    }
}
