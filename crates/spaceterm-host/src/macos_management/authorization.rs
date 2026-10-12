//! Helper-local Authorization Services rights. Production never exports this grant.

use crate::native_deployment::contracts::NativeFailure;
use crate::native_management::{
    controller::AuthorizationRoute,
    facade::{
        authorization::NativeGrantPolicy, caller::NativeCallerAuthentication,
        native_api::linked_function, operation_authority::NativeOperationRights,
    },
    helper_lifetime::HelperDeadline,
    request::ManagementRequest,
};
use std::{
    ffi::c_void,
    sync::{Arc, Mutex},
    time::Duration,
};

type Acquire = unsafe extern "C" fn(*const u8, u64, u32, *mut *mut c_void) -> u32;
type Recheck = unsafe extern "C" fn(*mut c_void, *const u8) -> u32;
type Release = unsafe extern "C" fn(*mut *mut c_void) -> u32;
#[derive(Clone, Copy)]
struct Api {
    acquire: Acquire,
    recheck: Recheck,
    release: Release,
}
impl Api {
    fn linked() -> Result<Self, NativeFailure> {
        // SAFETY: these are fixed functions from the management native header, not input selectors.
        unsafe {
            Ok(Self {
                acquire: linked_function(c"st_management_request_local_right")?,
                recheck: linked_function(c"st_management_local_right_recheck")?,
                release: linked_function(c"st_management_local_right_release")?,
            })
        }
    }
}
struct OwnedRight {
    native: *mut c_void,
    api: Api,
}
// SAFETY: the native handle is used only under the containing Mutex and is uniquely released.
unsafe impl Send for OwnedRight {}
impl Drop for OwnedRight {
    fn drop(&mut self) {
        // SAFETY: C owns this opaque allocation; release nulls it and destroys its exact rights.
        unsafe {
            (self.api.release)(&mut self.native);
        }
    }
}
/// Owned Authorization Services token, confined to one native helper and exact request.
pub struct LocalAuthorizationRight(Mutex<Option<OwnedRight>>);
impl std::fmt::Debug for LocalAuthorizationRight {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LocalAuthorizationRight(..)")
    }
}
/// Native audit-session attachment before authd interaction.
/// Implementations select only the retained kernel sender's session.
pub trait AuthorizationServicesCaller: NativeCallerAuthentication {
    /// Joins the original GUI caller's native audit session in this one-request helper.
    fn join_session(
        &self,
        caller: &Self::Caller,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure>;
}

/// Real native GUI authorization over an independently authenticated caller adapter.
pub struct AuthorizationServices<C> {
    callers: Arc<C>,
}
impl<C: AuthorizationServicesCaller> AuthorizationServices<C> {
    /// Keeps the same OS caller adapter that received the original request.
    pub fn new(callers: Arc<C>) -> Self {
        Self { callers }
    }
}
impl<C: AuthorizationServicesCaller> NativeOperationRights<C::Caller> for AuthorizationServices<C> {
    type Right = LocalAuthorizationRight;
    fn acquire(
        &self,
        caller: &C::Caller,
        request: &ManagementRequest,
        issued: Duration,
        route: AuthorizationRoute,
        deadline: HelperDeadline,
    ) -> Result<Self::Right, NativeFailure> {
        if route != AuthorizationRoute::Gui {
            return Err(NativeFailure::AuthorizationUnavailable);
        }
        self.callers.recheck(caller, deadline)?;
        let uid = self.callers.uid(caller)?;
        if uid != request.key().account_uid {
            return Err(NativeFailure::IdentityMismatch);
        }
        let issued = u64::try_from(issued.as_nanos()).map_err(|_| NativeFailure::LimitExceeded)?;
        self.callers.join_session(caller, deadline)?;
        let api = Api::linked()?;
        let mut native = std::ptr::null_mut();
        // SAFETY: the request is strictly decoded, issuance comes from the protected owner,
        // and the caller UID comes from the retained native principal. C retains no input pointer.
        let result = unsafe { (api.acquire)(request.encode().as_ptr(), issued, uid, &mut native) };
        let owned = OwnedRight { native, api };
        classify(result)?;
        if owned.native.is_null() {
            return Err(NativeFailure::AuthorizationUnavailable);
        }
        let right = LocalAuthorizationRight(Mutex::new(Some(owned)));
        self.recheck(caller, &right, request, deadline)?;
        Ok(right)
    }
    fn policy(&self, _: &Self::Right) -> Result<NativeGrantPolicy, NativeFailure> {
        // The native adapter inspects the complete authd rule before acquiring and rechecking.
        Ok(NativeGrantPolicy::PasswordAuthenticated)
    }
    fn recheck(
        &self,
        caller: &C::Caller,
        right: &Self::Right,
        request: &ManagementRequest,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        self.callers.recheck(caller, deadline)?;
        if self.callers.uid(caller)? != request.key().account_uid {
            return Err(NativeFailure::IdentityMismatch);
        }
        let guard = right
            .0
            .lock()
            .map_err(|_| NativeFailure::AuthorizationUnavailable)?;
        let native = guard.as_ref().ok_or(NativeFailure::AuthorizationDenied)?;
        // SAFETY: the Mutex retains the owned native token and the exact request buffer through inspection.
        classify(unsafe { (native.api.recheck)(native.native, request.encode().as_ptr()) })?;
        self.callers.recheck(caller, deadline)
    }
    fn revoke(&self, right: &mut Self::Right) {
        if let Ok(native) = right.0.get_mut() {
            native.take();
        }
    }
}
fn classify(code: u32) -> Result<(), NativeFailure> {
    if code == 0 {
        Ok(())
    } else {
        Err(NativeFailure::try_from(code)?)
    }
}
