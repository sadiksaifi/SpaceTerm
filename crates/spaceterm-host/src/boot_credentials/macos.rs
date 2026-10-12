//! Fixed System Keychain operations. Native scopes come from the installation
//! owner, not from decoded profile facts. Missing owner or C linkage fails closed.

use crate::{
    boot_credentials::{BootCredentialAdapter, CredentialPolicyFacts, LockedSecret32},
    native_deployment::{
        authority::NativeTransactionLease,
        contracts::{BootCredentialBinding, CredentialReleasePermit, NativeFailure},
    },
    native_management::facade::native_api::linked_function,
};
use std::{ffi::c_void, ptr::NonNull};

/// Boot-credential adapter with no caller-selected item, Keychain or executable.
#[derive(Default)]
pub struct MacosBootCredentialAdapter;

impl BootCredentialAdapter for MacosBootCredentialAdapter {
    fn inspect_policy(
        &self,
        binding: &BootCredentialBinding,
    ) -> Result<CredentialPolicyFacts, NativeFailure> {
        let digest = binding.sha256()?;
        type Issue = unsafe extern "C" fn(*const u8, *mut *mut c_void) -> u32;
        // SAFETY: only this static owner callback has this exact opaque-scope ABI.
        let issue: Issue = unsafe { linked_function(c"st_native_keychain_inspection_scope")? };
        let item = NativeItem::retain(|out| {
            // SAFETY: digest is bounded comparison data. The issuer independently
            // reads and retains protected registration; the digest grants no authority.
            unsafe { issue(digest.as_ptr(), out) }
        })?;
        type Inspect = unsafe extern "C" fn(*const c_void, *mut KeychainPolicyObservation) -> u32;
        // SAFETY: this is the static C item-inspection ABI.
        let inspect: Inspect = unsafe { linked_function(c"st_keychain_inspect_fixed_item")? };
        let mut observation = KeychainPolicyObservation::default();
        // SAFETY: item owns the retained C binding and its original owner scope.
        native_result(unsafe { inspect(item.binding.as_ptr(), &mut observation) })?;
        observation.validate()?;
        Ok(CredentialPolicyFacts {
            schema: 1,
            account_and_code_bound: true,
            interaction_disabled: true,
        })
    }

    fn retrieve_for_startup(
        &self,
        permit: &CredentialReleasePermit,
        destination: &mut LockedSecret32,
    ) -> Result<(), NativeFailure> {
        type Retrieve = unsafe extern "C" fn(*const c_void, *mut c_void) -> u32;
        destination.wipe();
        let result = (|| {
            // SAFETY: C treats both pointers as opaque. The common Rust/native
            // bridge alone rechecks the permit and accesses locked destination memory.
            let retrieve: Retrieve =
                unsafe { linked_function(c"st_keychain_retrieve_for_startup")? };
            native_result(unsafe {
                retrieve(
                    std::ptr::from_ref(permit).cast(),
                    std::ptr::from_mut(destination).cast(),
                )
            })
        })();
        if result.is_err() {
            destination.wipe();
        }
        result
    }
}

/// Narrow item lifecycle under the installation owner's existing transaction.
/// No receipt decoder, arbitrary selector or alternative Keychain grants authority.
#[derive(Default)]
pub struct MacosKeychainItemLifecycle;

impl MacosKeychainItemLifecycle {
    /// Enrolls only the lease's newly reserved item. Its native owner records the
    /// persistent item identity before policy inspection. Failure never reuses its slot.
    pub fn enroll(&self, lease: &NativeTransactionLease) -> Result<(), NativeFailure> {
        self.apply(lease, 2, c"st_keychain_enroll_owned_item")
    }

    /// Removes only the lease's exact protected item receipt, retaining that native
    /// item through deletion. Partial installation never permits deletion by label.
    pub fn remove(&self, lease: &NativeTransactionLease) -> Result<(), NativeFailure> {
        self.apply(lease, 3, c"st_keychain_remove_owned_item")
    }

    fn apply(
        &self,
        lease: &NativeTransactionLease,
        operation: u32,
        name: &std::ffi::CStr,
    ) -> Result<(), NativeFailure> {
        type Issue = unsafe extern "C" fn(*const c_void, u32, *mut *mut c_void) -> u32;
        type Apply = unsafe extern "C" fn(*mut c_void) -> u32;
        // SAFETY: the owner callback treats the Rust lease as opaque; it must
        // bind its native scope to this live transaction and exact operation.
        let issue: Issue = unsafe { linked_function(c"st_native_keychain_installation_scope")? };
        // Resolve the operation before issuing a scope, including on missing linkage.
        let apply: Apply = unsafe { linked_function(name)? };
        let item = NativeItem::retain(|out| unsafe {
            issue(std::ptr::from_ref(lease).cast(), operation, out)
        })?;
        // SAFETY: the lease, scope and exact C binding remain live through the operation.
        native_result(unsafe { apply(item.binding.as_ptr()) })
    }
}

type ReleaseScope = unsafe extern "C" fn(*mut *mut c_void);
type ReleaseBinding = unsafe extern "C" fn(*mut *mut c_void);

struct NativeItem {
    binding: NonNull<c_void>,
    scope: NonNull<c_void>,
    release_binding: ReleaseBinding,
    release_scope: ReleaseScope,
}

impl NativeItem {
    fn retain(issue: impl FnOnce(*mut *mut c_void) -> u32) -> Result<Self, NativeFailure> {
        type Retain = unsafe extern "C" fn(*const c_void, *mut *mut c_void) -> u32;
        // SAFETY: static functions from the protected native closure, with the
        // exact opaque-pointer ABI. Resolve cleanup before acquiring any handle.
        let (release_binding, release_scope, retain): (ReleaseBinding, ReleaseScope, Retain) = unsafe {
            (
                linked_function(c"st_keychain_release_binding")?,
                linked_function(c"st_native_keychain_scope_release")?,
                linked_function(c"st_keychain_retain_scope")?,
            )
        };
        let mut scope = std::ptr::null_mut();
        let mut binding = std::ptr::null_mut();
        let result = (|| {
            native_result(issue(&mut scope))?;
            let scope_handle = NonNull::new(scope).ok_or(NativeFailure::NativeUnavailable)?;
            // SAFETY: the owner issued this scope; C copies comparison data only
            // after its guard. The scope remains owned until binding release.
            native_result(unsafe { retain(scope, &mut binding) })?;
            let binding_handle = NonNull::new(binding).ok_or(NativeFailure::NativeUnavailable)?;
            Ok((binding_handle, scope_handle))
        })();
        match result {
            Ok((binding, scope)) => Ok(Self {
                binding,
                scope,
                release_binding,
                release_scope,
            }),
            Err(failure) => {
                // SAFETY: native copy operations own outputs even on failure. Their
                // release functions accept NULL. Binding must retire before its scope.
                unsafe {
                    release_binding(&mut binding);
                    release_scope(&mut scope);
                }
                Err(failure)
            }
        }
    }
}

impl Drop for NativeItem {
    fn drop(&mut self) {
        let mut binding = self.binding.as_ptr();
        let mut scope = self.scope.as_ptr();
        // SAFETY: this instance uniquely owns both handles; the binding borrows
        // the original scope, so their release order is intentional.
        unsafe {
            (self.release_binding)(&mut binding);
            (self.release_scope)(&mut scope);
        }
    }
}

fn native_result(result: u32) -> Result<(), NativeFailure> {
    if result == 0 {
        Ok(())
    } else {
        Err(NativeFailure::try_from(result).unwrap_or(NativeFailure::InvalidRecord))
    }
}

/// In-process C observation ABI. It contains no attributes or credential reference.
#[derive(Clone, Copy, Default)]
#[repr(C)]
struct KeychainPolicyObservation {
    schema: u32,
    policy_class: u32,
    interaction_class: u32,
    account_binding_matches: u32,
    code_binding_matches: u32,
    auxiliary_acl_class: u32,
}

const _: () = {
    assert!(size_of::<KeychainPolicyObservation>() == 24);
    assert!(align_of::<KeychainPolicyObservation>() == 4);
    assert!(std::mem::offset_of!(KeychainPolicyObservation, auxiliary_acl_class) == 20);
};

impl KeychainPolicyObservation {
    fn validate(self) -> Result<(), NativeFailure> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_or_weakened_observation_is_never_native_policy_success() {
        for value in [0, 2, u32::MAX] {
            for field in 0..6 {
                let mut values = [1; 6];
                values[field] = value;
                let [
                    schema,
                    policy_class,
                    interaction_class,
                    account_binding_matches,
                    code_binding_matches,
                    auxiliary_acl_class,
                ] = values;
                assert_eq!(
                    KeychainPolicyObservation {
                        schema,
                        policy_class,
                        interaction_class,
                        account_binding_matches,
                        code_binding_matches,
                        auxiliary_acl_class,
                    }
                    .validate(),
                    Err(NativeFailure::CredentialPolicyRejected)
                );
            }
        }
    }
}
