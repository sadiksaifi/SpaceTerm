//! Fixed native boot-credential port and locked, wiped secret ownership.

use crate::native_deployment::{contracts::NativeFailure, profile::BootCredentialBinding};
use std::{
    fmt,
    ptr::NonNull,
    sync::atomic::{Ordering, compiler_fence},
};

#[cfg(target_os = "linux")]
pub mod linux;

/// Noncloneable credential-release authority. Facts and wire recognition cannot construct it.
pub struct CredentialReleasePermit {
    lease: Box<dyn RetainedCredentialRelease>,
}

/// Issued only by native-authority's retained serialized release fence. The
/// lease must keep genuine authenticated publication and credential custody
/// live, not reconstruct authority from the context or credential digest.
pub(crate) trait RetainedCredentialRelease: Send + Sync {
    fn context(&self) -> &crate::startup::PendingStartupContext;
    fn credential_binding(&self) -> &BootCredentialBinding;
    fn recheck(&self) -> Result<(), NativeFailure>;
    #[cfg(target_os = "linux")]
    fn linux_execution(&self) -> Result<linux::RetainedCredentialExecution<'_>, NativeFailure>;
}

impl CredentialReleasePermit {
    #[cfg_attr(
        not(all(target_os = "linux", feature = "test-support")),
        expect(
            dead_code,
            reason = "native-authority A3 supplies the retained fence issuer"
        )
    )]
    pub(crate) fn retain(lease: impl RetainedCredentialRelease + 'static) -> Self {
        Self {
            lease: Box::new(lease),
        }
    }

    /// Exact context retained under the permit's serialized native release fence.
    pub fn context(&self) -> &crate::startup::PendingStartupContext {
        self.lease.context()
    }

    /// Fixed native credential binding retained by the issuer, not a caller selector.
    pub fn credential_binding(&self) -> &BootCredentialBinding {
        self.lease.credential_binding()
    }

    /// Revalidates current authority while the original retained fence remains held.
    pub fn recheck(&self) -> Result<(), NativeFailure> {
        if self.credential_binding().sha256()? != self.context().credential_binding_sha256 {
            return Err(NativeFailure::CredentialPolicyRejected);
        }
        self.lease.recheck()
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn linux_execution(
        &self,
    ) -> Result<linux::RetainedCredentialExecution<'_>, NativeFailure> {
        self.recheck()?;
        self.lease.linux_execution()
    }
}

/// Native binding lookup over the permit's owned retained release lease. The
/// coordinator selects this adapter only with the genuine native fence issuer.
pub struct RetainedCredentialReleaseBindings;
impl crate::native_deployment::ports::CredentialReleaseBindings
    for RetainedCredentialReleaseBindings
{
    fn context<'a>(
        &self,
        permit: &'a CredentialReleasePermit,
    ) -> Result<&'a crate::startup::PendingStartupContext, NativeFailure> {
        permit.recheck()?;
        Ok(permit.context())
    }

    fn credential_binding<'a>(
        &self,
        permit: &'a CredentialReleasePermit,
    ) -> Result<&'a BootCredentialBinding, NativeFailure> {
        permit.recheck()?;
        Ok(permit.credential_binding())
    }
}

/// Bounded native policy observation. It contains no credential selector or secret.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CredentialPolicyFacts {
    /// Persisted policy schema observed by the native adapter.
    pub schema: u16,
    /// Native policy was checked as a conjunction with exact account/code binding.
    pub account_and_code_bound: bool,
    /// Native retrieval cannot prompt.
    pub interaction_disabled: bool,
}

/// Native credential access fixed by the protected profile and live release authority.
pub trait BootCredentialAdapter: Send + Sync {
    /// Inspects policy without reading secret bytes.
    fn inspect_policy(
        &self,
        binding: &BootCredentialBinding,
    ) -> Result<CredentialPolicyFacts, NativeFailure>;
    /// Retrieves only the fixed credential after native permit revalidation.
    fn retrieve_for_startup(
        &self,
        permit: &CredentialReleasePermit,
        destination: &mut LockedSecret32,
    ) -> Result<(), NativeFailure>;
}

/// Anonymous locked owned memory, wiped before unlock and unmap. It has no contents Debug.
pub struct LockedSecret32 {
    allocation: NonNull<u8>,
}
impl LockedSecret32 {
    /// Allocates anonymous memory and requires native locking, with no pageable fallback.
    pub fn new() -> Result<Self, NativeFailure> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            // SAFETY: anonymous private mapping, no source file or account pathname.
            let ptr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    32,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANON,
                    -1,
                    0,
                )
            };
            if ptr == libc::MAP_FAILED {
                return Err(NativeFailure::CredentialUnavailable);
            }
            let Some(allocation) = NonNull::new(ptr.cast::<u8>()) else {
                // SAFETY: ptr denotes the just-created anonymous mapping.
                unsafe {
                    libc::munmap(ptr, 32);
                }
                return Err(NativeFailure::CredentialUnavailable);
            };
            let secret = Self { allocation };
            // SAFETY: the entire requested region is owned and mapped.
            if unsafe { libc::mlock(ptr, 32) } != 0 {
                return Err(NativeFailure::CredentialUnavailable);
            }
            #[cfg(target_os = "linux")]
            // SAFETY: the mapping is page aligned; Linux accepts a rounded region length.
            if unsafe { libc::madvise(ptr, 32, libc::MADV_DONTDUMP) } != 0
                || unsafe { libc::madvise(ptr, 32, libc::MADV_WIPEONFORK) } != 0
            {
                return Err(NativeFailure::CredentialUnavailable);
            }
            Ok(secret)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err(NativeFailure::UnsupportedHost)
        }
    }
    /// Writes exactly one credential into the already locked mapping. This is
    /// memory ownership only; it never grants credential-release authority.
    pub fn write(&mut self, bytes: &[u8]) -> Result<(), NativeFailure> {
        if bytes.len() != 32 {
            self.wipe();
            return Err(NativeFailure::CredentialPolicyRejected);
        }
        // SAFETY: the owned mapping has exactly 32 writable secret bytes.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.allocation.as_ptr(), 32) };
        Ok(())
    }

    pub(crate) fn bytes(&self) -> &[u8; 32] {
        // SAFETY: the live mapping is initialized and contains exactly 32 bytes.
        unsafe { &*self.allocation.as_ptr().cast::<[u8; 32]>() }
    }

    /// Wipes the exact secret region without releasing its ownership.
    pub fn wipe(&mut self) {
        // SAFETY: allocation is owned, mapped and writable for its full lifetime.
        for i in 0..32 {
            unsafe {
                self.allocation.as_ptr().add(i).write_volatile(0);
            }
        }
        compiler_fence(Ordering::SeqCst);
    }
}
impl fmt::Debug for LockedSecret32 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LockedSecret32(..)")
    }
}
impl Drop for LockedSecret32 {
    fn drop(&mut self) {
        self.wipe();
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        // SAFETY: this instance uniquely owns its anonymous mapping.
        unsafe {
            libc::munlock(self.allocation.as_ptr().cast(), 32);
            libc::munmap(self.allocation.as_ptr().cast(), 32);
        }
    }
}

/// Fixed unavailable adapter for platforms whose native exchange has not been bound.
#[derive(Default)]
pub struct UnavailableBootCredentials;
impl BootCredentialAdapter for UnavailableBootCredentials {
    fn inspect_policy(
        &self,
        _binding: &BootCredentialBinding,
    ) -> Result<CredentialPolicyFacts, NativeFailure> {
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

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;
    #[test]
    fn owned_locked_memory_wipe_clears_every_byte_without_debug_contents() {
        let mut secret = LockedSecret32::new().unwrap();
        // SAFETY: this fixture owns the 32-byte mapping and uses only a synthetic marker.
        unsafe {
            std::ptr::write_bytes(secret.allocation.as_ptr(), 17, 32);
        }
        assert_eq!(format!("{secret:?}"), "LockedSecret32(..)");
        secret.wipe();
        // SAFETY: wiping preserves ownership and mapping until Drop.
        let bytes = unsafe { std::slice::from_raw_parts(secret.allocation.as_ptr(), 32) };
        assert!(bytes.iter().all(|b| *b == 0));
    }
}
