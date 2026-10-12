//! Native system-bus authorization of the retained kernel-authenticated account process.

use super::polkit::{NativePolkitPeer, PolkitNativeOutcome};
use crate::native_deployment::contracts::NativeFailure;
use crate::native_management::{
    controller::AuthorizationRoute,
    facade::{
        authorization::NativeGrantPolicy,
        caller::NativeCallerAuthentication,
        native_api::linked_function,
        socket_caller::{BrokerCaller, BrokerCallerPeer},
    },
    helper_lifetime::HelperDeadline,
    request::{ManagementRequest, authorization_right},
};
use std::{ffi::c_void, sync::Arc};

/// The custody owner's retained, supported effective polkit and protected system-bus policy.
/// It includes the OS policy restricting ownership of org.freedesktop.PolicyKit1 to polkitd.
/// XML defaults alone cannot prove absence of overriding JavaScript or retained grants.
pub trait NativePolkitPolicy: Send + Sync {
    /// Requires this exact action to use authentication without auth_admin_keep or standing YES rules.
    fn recheck(&self, request: &ManagementRequest) -> Result<(), NativeFailure>;
}

type Check = unsafe extern "C" fn(*const u8, u32, u32, u64, u64, *mut *mut c_void) -> u32;
type Recheck = unsafe extern "C" fn(*const c_void, *const u8, u32, u32, u64) -> u32;
type Release = unsafe extern "C" fn(*mut *mut c_void);
#[derive(Clone, Copy)]
struct Api {
    check: Check,
    recheck: Recheck,
    release: Release,
}
impl Api {
    fn linked() -> Result<Self, NativeFailure> {
        // SAFETY: every signature is the fixed native/linux/management/polkit.h ABI.
        unsafe {
            Ok(Self {
                check: linked_function(c"st_management_polkit_check")?,
                recheck: linked_function(c"st_management_polkit_recheck")?,
                release: linked_function(c"st_management_polkit_release")?,
            })
        }
    }
}
struct OwnedAuthorization {
    native: *mut c_void,
    api: Api,
}
// SAFETY: the native object is immutable after CheckAuthorization; destruction is unique.
unsafe impl Send for OwnedAuthorization {}
// SAFETY: recheck reads only the immutable subject/request fields and owned native reply.
unsafe impl Sync for OwnedAuthorization {}
impl Drop for OwnedAuthorization {
    fn drop(&mut self) {
        // SAFETY: C owns this opaque allocation and consumes/nulls it on release.
        unsafe {
            (self.api.release)(&mut self.native);
        }
    }
}
/// Native reply retained for one exact subject, action and request. It is never exported.
pub struct SystemPolkitGrant {
    native: Option<OwnedAuthorization>,
    request: ManagementRequest,
}
impl std::fmt::Debug for SystemPolkitGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SystemPolkitGrant(..)")
    }
}
/// Retained native process subject, supplied by the kernel caller adapter.
/// PID and start ticks are observations from that retained subject, never request fields.
pub trait NativePolkitProcessCaller: NativeCallerAuthentication {
    /// Rechecks and returns the original pidfd-bound process and native start time.
    fn process(&self, caller: &Self::Caller) -> Result<(u32, u64), NativeFailure>;
}
impl NativePolkitProcessCaller for BrokerCallerPeer {
    fn process(&self, caller: &BrokerCaller) -> Result<(u32, u64), NativeFailure> {
        Ok((caller.observed_pid(), caller.start_ticks()?))
    }
}
/// Real polkit peer. Neither PKEXEC_UID nor the privileged helper is the polkit subject.
pub struct SystemPolkitPeer<C = BrokerCallerPeer> {
    callers: Arc<C>,
    policy: Arc<dyn NativePolkitPolicy>,
}
impl<C: NativePolkitProcessCaller> SystemPolkitPeer<C> {
    /// Retains the actual writer and the custody owner's effective authentication policy.
    pub fn new(callers: Arc<C>, policy: Arc<dyn NativePolkitPolicy>) -> Self {
        Self { callers, policy }
    }
    fn check(
        &self,
        subject: &C::Caller,
        request: &ManagementRequest,
        action: &str,
        deadline: HelperDeadline,
    ) -> Result<SystemPolkitGrant, NativeFailure> {
        self.recheck_subject(subject, deadline)?;
        if self.uid(subject)? != request.key().account_uid
            || action != authorization_right(request.key().build_identity, request.operation())
        {
            return Err(NativeFailure::IdentityMismatch);
        }
        self.policy.recheck(request)?;
        let api = Api::linked()?;
        let mut native = std::ptr::null_mut();
        let timeout = u64::try_from(deadline.remaining()?.as_micros())
            .map_err(|_| NativeFailure::LimitExceeded)?;
        // SAFETY: the native subject comes from SCM_CREDENTIALS plus the retained pidfd,
        // not caller fields. C retains the genuine system authority's reply, not these buffers.
        let (pid, start) = self.callers.process(subject)?;
        let result = unsafe {
            (api.check)(
                request.encode().as_ptr(),
                self.uid(subject)?,
                pid,
                start,
                timeout,
                &mut native,
            )
        };
        let owned = OwnedAuthorization { native, api };
        classify(result)?;
        if owned.native.is_null() {
            return Err(NativeFailure::AuthorizationUnavailable);
        }
        let grant = SystemPolkitGrant {
            native: Some(owned),
            request: request.clone(),
        };
        self.recheck_grant(subject, &grant, request, action, deadline)?;
        Ok(grant)
    }
}
impl<C: NativePolkitProcessCaller> NativePolkitPeer for SystemPolkitPeer<C> {
    type Subject = C::Caller;
    type Grant = SystemPolkitGrant;
    fn subject(&self, deadline: HelperDeadline) -> Result<Self::Subject, NativeFailure> {
        self.callers.bind(AuthorizationRoute::Gui, deadline)
    }
    fn uid(&self, subject: &Self::Subject) -> Result<u32, NativeFailure> {
        self.callers.uid(subject)
    }
    fn recheck_subject(
        &self,
        subject: &Self::Subject,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        self.callers.recheck(subject, deadline)
    }
    fn authorize(
        &self,
        subject: &Self::Subject,
        request: &ManagementRequest,
        action: &str,
        deadline: HelperDeadline,
    ) -> PolkitNativeOutcome<Self::Grant> {
        match self.check(subject, request, action, deadline) {
            Ok(grant) => PolkitNativeOutcome::Granted(grant),
            Err(NativeFailure::AuthorizationDenied) => PolkitNativeOutcome::Denied,
            Err(NativeFailure::AuthorizationCancelled) => PolkitNativeOutcome::Cancelled,
            Err(_) => PolkitNativeOutcome::Unavailable,
        }
    }
    fn policy(&self, grant: &Self::Grant) -> Result<NativeGrantPolicy, NativeFailure> {
        self.policy.recheck(&grant.request)?;
        Ok(NativeGrantPolicy::PasswordAuthenticated)
    }
    fn recheck_grant(
        &self,
        subject: &Self::Subject,
        grant: &Self::Grant,
        request: &ManagementRequest,
        action: &str,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        self.recheck_subject(subject, deadline)?;
        if request != &grant.request
            || action != authorization_right(request.key().build_identity, request.operation())
        {
            return Err(NativeFailure::IdentityMismatch);
        }
        self.policy.recheck(request)?;
        let native = grant
            .native
            .as_ref()
            .ok_or(NativeFailure::AuthorizationDenied)?;
        // SAFETY: the retained native reply and original kernel subject stay live for this inspection.
        let (pid, start) = self.callers.process(subject)?;
        classify(unsafe {
            (native.api.recheck)(
                native.native,
                request.encode().as_ptr(),
                self.uid(subject)?,
                pid,
                start,
            )
        })?;
        self.recheck_subject(subject, deadline)
    }
    fn revoke(&self, grant: &mut Self::Grant) {
        grant.native.take();
    }
}
fn classify(code: u32) -> Result<(), NativeFailure> {
    if code == 0 {
        Ok(())
    } else {
        Err(NativeFailure::try_from(code)?)
    }
}
