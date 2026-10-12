//! launchd demand-helper caller binding. C retains the kernel audit token and dynamic SecCode guest.

#[cfg(target_os = "macos")]
mod native {
    use crate::native_deployment::contracts::NativeFailure;
    use crate::native_management::{
        controller::AuthorizationRoute, facade::caller::NativeCallerAuthentication,
        helper_lifetime::HelperDeadline, request::ManagementRequest,
    };
    use crate::startup::BuildIdentity;
    use std::{ffi::c_void, ptr::NonNull, sync::Arc};
    use zeroize::Zeroizing;

    unsafe extern "C" {
        fn st_management_receive_gui_demand(
            build: u32,
            remaining_ms: u32,
            out: *mut *mut c_void,
        ) -> u32;
        fn st_management_demand_request(message: *const c_void, out: *mut u8) -> u32;
        fn st_management_demand_caller_uid(message: *const c_void, out: *mut u32) -> u32;
        fn st_management_demand_external(message: *const c_void, out: *mut u8) -> u32;
        fn st_management_demand_recheck(message: *const c_void) -> u32;
        fn st_management_demand_release(message: *mut *mut c_void);
    }
    struct OwnedDemand(NonNull<c_void>);
    // SAFETY: C's owned message is immutable after receive, retaining audit-token
    // identity and CF references. All getters/rechecks borrow it; Drop is unique.
    unsafe impl Send for OwnedDemand {}
    // SAFETY: readonly Security guest inspection and immutable audit data retain
    // no Rust buffers. The Arc prevents concurrent destruction of the C object.
    unsafe impl Sync for OwnedDemand {}
    impl Drop for OwnedDemand {
        fn drop(&mut self) {
            let mut native = self.0.as_ptr();
            // SAFETY: this is the unique C allocation returned by receive.
            unsafe {
                st_management_demand_release(&mut native);
            }
        }
    }

    /// Retained actual sender. It has no constructor from an audit token, UID, PID, or request facts.
    #[derive(Clone)]
    pub struct MachDemandCaller(Arc<OwnedDemand>);
    impl std::fmt::Debug for MachDemandCaller {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("MachDemandCaller(..)")
        }
    }
    impl MachDemandCaller {
        /// The saved form carried in the original authenticated native message.
        /// It is sensitive authority, not caller proof; the protected owner must enroll and consume it.
        pub fn external_form(&self) -> Result<Zeroizing<[u8; 32]>, NativeFailure> {
            let mut bytes = Zeroizing::new([0; 32]);
            // SAFETY: the retained opaque message and exact initialized 32-byte buffer are live.
            classify(unsafe {
                st_management_demand_external(self.0.0.as_ptr(), bytes.as_mut_ptr())
            })?;
            Ok(bytes)
        }
    }

    /// Actual one-request receiver and audit-token principal adapter.
    pub struct MachDemandCallerPeer {
        caller: MachDemandCaller,
    }
    impl MachDemandCallerPeer {
        /// Checks in to the fixed build's launchd endpoint and receives one kernel-audited request.
        /// The supplied build is the compiled helper identity, never a request-selected namespace.
        pub fn receive(
            build: BuildIdentity,
            deadline: HelperDeadline,
        ) -> Result<(Self, ManagementRequest), NativeFailure> {
            let remaining = deadline.remaining()?.as_millis().clamp(1, 2000) as u32;
            let build = match build {
                BuildIdentity::Release => 1,
                BuildIdentity::Preflight => 2,
                BuildIdentity::Development => 3,
            };
            let mut pointer = std::ptr::null_mut();
            // SAFETY: C initializes the one owned result pointer and accepts no remote pointers.
            classify(unsafe { st_management_receive_gui_demand(build, remaining, &mut pointer) })?;
            let owned =
                OwnedDemand(NonNull::new(pointer).ok_or(NativeFailure::AuthorizationUnavailable)?);
            let mut bytes = [0; 160];
            // SAFETY: the owned opaque message and exact output buffer remain live through the call.
            classify(unsafe {
                st_management_demand_request(owned.0.as_ptr(), bytes.as_mut_ptr())
            })?;
            let request = ManagementRequest::decode(&bytes)?;
            deadline.check()?;
            Ok((
                Self {
                    caller: MachDemandCaller(Arc::new(owned)),
                },
                request,
            ))
        }
    }
    impl NativeCallerAuthentication for MachDemandCallerPeer {
        type Caller = MachDemandCaller;
        fn bind(
            &self,
            route: AuthorizationRoute,
            deadline: HelperDeadline,
        ) -> Result<Self::Caller, NativeFailure> {
            if route != AuthorizationRoute::Gui {
                return Err(NativeFailure::AuthorizationUnavailable);
            }
            self.recheck(&self.caller, deadline)?;
            Ok(self.caller.clone())
        }
        fn uid(&self, caller: &Self::Caller) -> Result<u32, NativeFailure> {
            let mut uid = 0;
            // SAFETY: caller owns the opaque native object and uid is an initialized output.
            classify(unsafe { st_management_demand_caller_uid(caller.0.0.as_ptr(), &mut uid) })?;
            Ok(uid)
        }
        fn recheck(
            &self,
            caller: &Self::Caller,
            deadline: HelperDeadline,
        ) -> Result<(), NativeFailure> {
            deadline.check()?;
            // SAFETY: caller owns the kernel-audit-bound native message for this readonly inspection.
            classify(unsafe { st_management_demand_recheck(caller.0.0.as_ptr()) })?;
            deadline.check()
        }
    }
    fn classify(code: u32) -> Result<(), NativeFailure> {
        match code {
            0 => Ok(()),
            9 => Err(NativeFailure::InvalidRecord),
            12 => Err(NativeFailure::IdentityMismatch),
            16 => Err(NativeFailure::AuthorizationDenied),
            29 => Err(NativeFailure::TimedOut),
            _ => Err(NativeFailure::AuthorizationUnavailable),
        }
    }
}
#[cfg(target_os = "macos")]
pub use native::{MachDemandCaller, MachDemandCallerPeer};
