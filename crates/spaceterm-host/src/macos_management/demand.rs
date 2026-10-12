//! One-request launchd transport with kernel-audited callers and send-once replies.

#[cfg(target_os = "macos")]
mod native {
    use crate::native_deployment::{
        contracts::NativeFailure, production_gate::require_native_evidence,
    };
    use crate::native_management::{
        cli_sudo::reply,
        controller::{AuthorizationRoute, ManagementProgress},
        facade::{
            ConfirmedNativeAction, caller::NativeCallerAuthentication, native_api::linked_function,
            transport::NativeGuiManagementTransport,
        },
        helper_lifetime::HelperDeadline,
        request::ManagementRequest,
    };
    use crate::{native_deployment::contracts::NativeManagementFuture, startup::BuildIdentity};
    use std::{
        ffi::c_void,
        ptr::NonNull,
        sync::{Arc, Mutex},
    };
    use zeroize::Zeroizing;

    type Receive = unsafe extern "C" fn(u32, u32, *mut *mut c_void) -> u32;
    type Bytes = unsafe extern "C" fn(*const c_void, *mut u8) -> u32;
    type Uid = unsafe extern "C" fn(*const c_void, *mut u32) -> u32;
    type Recheck = unsafe extern "C" fn(*const c_void) -> u32;
    type Release = unsafe extern "C" fn(*mut *mut c_void);
    type Reply = unsafe extern "C" fn(*mut c_void, *const u8, usize, u32) -> u32;
    #[derive(Clone, Copy)]
    struct Api {
        receive: Receive,
        request: Bytes,
        uid: Uid,
        external: Bytes,
        recheck: Recheck,
        release: Release,
        reply: Reply,
    }
    impl Api {
        fn linked() -> Result<Self, NativeFailure> {
            // SAFETY: every ABI is the exact fixed signature in native/macos/management.h.
            unsafe {
                Ok(Self {
                    receive: linked_function(c"st_management_receive_gui_demand")?,
                    request: linked_function(c"st_management_demand_request")?,
                    uid: linked_function(c"st_management_demand_caller_uid")?,
                    external: linked_function(c"st_management_demand_external")?,
                    recheck: linked_function(c"st_management_demand_recheck")?,
                    release: linked_function(c"st_management_demand_release")?,
                    reply: linked_function(c"st_management_demand_reply")?,
                })
            }
        }
    }
    struct OwnedDemand {
        native: NonNull<c_void>,
        api: Api,
        reply: Mutex<()>,
    }
    // SAFETY: C's audit/code data is immutable; its reply port is protected by reply's Mutex.
    // Arc retains the object through every caller observation and the unique send.
    unsafe impl Send for OwnedDemand {}
    // SAFETY: principal getters borrow immutable fields. The reply Mutex serializes
    // mutation of C's disjoint send-once field, and Drop requires unique ownership.
    unsafe impl Sync for OwnedDemand {}
    impl Drop for OwnedDemand {
        fn drop(&mut self) {
            let mut native = self.native.as_ptr();
            // SAFETY: this is the unique allocation returned by the linked native receiver.
            unsafe {
                (self.api.release)(&mut native);
            }
        }
    }
    /// Retained native sender. It has no constructor from UID, PID, audit bytes or request facts.
    #[derive(Clone)]
    pub struct MachDemandCaller(Arc<OwnedDemand>);
    impl std::fmt::Debug for MachDemandCaller {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("MachDemandCaller(..)")
        }
    }
    impl MachDemandCaller {
        /// Legacy saved form, never selected by the helper-local Authorization Services route.
        pub fn external_form(&self) -> Result<Zeroizing<[u8; 32]>, NativeFailure> {
            let mut bytes = Zeroizing::new([0; 32]);
            // SAFETY: the retained native object and exact initialized buffer remain live.
            classify(unsafe { (self.0.api.external)(self.0.native.as_ptr(), bytes.as_mut_ptr()) })?;
            Ok(bytes)
        }
    }
    /// Native receiver, caller adapter and original send-once reply owner.
    pub struct MachDemandCallerPeer {
        caller: MachDemandCaller,
    }
    impl MachDemandCallerPeer {
        /// Checks in to the compiled build's launchd endpoint and receives one kernel-audited request.
        pub fn receive(
            build: BuildIdentity,
            deadline: HelperDeadline,
        ) -> Result<(Self, ManagementRequest), NativeFailure> {
            let remaining = deadline.remaining()?.as_millis().clamp(1, 2000) as u32;
            let api = Api::linked()?;
            let mut pointer = std::ptr::null_mut();
            // SAFETY: C returns one owned opaque object and retains no Rust pointer.
            classify(unsafe { (api.receive)(build_code(build), remaining, &mut pointer) })?;
            let owned = OwnedDemand {
                native: NonNull::new(pointer).ok_or(NativeFailure::AuthorizationUnavailable)?,
                api,
                reply: Mutex::new(()),
            };
            let mut bytes = [0; 160];
            // SAFETY: the opaque object and exact request buffer remain live through the call.
            classify(unsafe { (api.request)(owned.native.as_ptr(), bytes.as_mut_ptr()) })?;
            let request = ManagementRequest::decode(&bytes)?;
            deadline.check()?;
            Ok((
                Self {
                    caller: MachDemandCaller(Arc::new(owned)),
                },
                request,
            ))
        }
        /// Sends one bounded result and retains the helper until the authenticated client
        /// copies it and acknowledges the original scope, within the same lifetime budget.
        pub fn reply(
            &self,
            result: Result<ManagementProgress, NativeFailure>,
            deadline: HelperDeadline,
        ) -> Result<(), NativeFailure> {
            let _serial = self
                .caller
                .0
                .reply
                .lock()
                .map_err(|_| NativeFailure::AuthorizationUnavailable)?;
            let bytes = match result {
                Ok(progress) => reply::encode(progress),
                Err(failure) => format!("classification={}\n", failure as u32),
            };
            let remaining = deadline
                .remaining()?
                .as_millis()
                .clamp(1, u128::from(u32::MAX)) as u32;
            // SAFETY: only this unique receiver can move the original native reply right.
            // C retains the one-request helper through its scoped acknowledgement wait.
            classify(unsafe {
                (self.caller.0.api.reply)(
                    self.caller.0.native.as_ptr(),
                    bytes.as_ptr(),
                    bytes.len(),
                    remaining,
                )
            })
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
            // SAFETY: the retained opaque object and exact uid output are live.
            classify(unsafe { (caller.0.api.uid)(caller.0.native.as_ptr(), &mut uid) })?;
            Ok(uid)
        }
        fn recheck(
            &self,
            caller: &Self::Caller,
            deadline: HelperDeadline,
        ) -> Result<(), NativeFailure> {
            deadline.check()?;
            // SAFETY: caller owns the kernel-audit-bound message through this readonly inspection.
            classify(unsafe { (caller.0.api.recheck)(caller.0.native.as_ptr()) })?;
            deadline.check()
        }
    }
    impl super::super::authorization::AuthorizationServicesCaller for MachDemandCallerPeer {
        fn join_session(
            &self,
            caller: &MachDemandCaller,
            deadline: HelperDeadline,
        ) -> Result<(), NativeFailure> {
            self.recheck(caller, deadline)?;
            // SAFETY: this fixed symbol accepts only the retained native message, not session facts.
            let join: Recheck = unsafe { linked_function(c"st_management_demand_join_session")? };
            // SAFETY: the native message retains the original kernel audit token through the call.
            classify(unsafe { join(caller.0.native.as_ptr()) })?;
            self.recheck(caller, deadline)
        }
    }

    /// Fixed GUI client route. It sends no password, executable selector or exported grant.
    pub struct MachGuiManagement;
    impl NativeGuiManagementTransport for MachGuiManagement {
        fn execute(
            &self,
            confirmed: ConfirmedNativeAction,
        ) -> NativeManagementFuture<'_, ManagementProgress> {
            Box::pin(async move {
                require_native_evidence()?;
                let request = confirmed.assessment().request().clone();
                let (reply, waiting) = tokio::sync::oneshot::channel();
                std::thread::Builder::new()
                    .name("native-gui-management".into())
                    .spawn(move || {
                        let _ = reply.send(roundtrip(&request));
                    })
                    .map_err(|_| NativeFailure::NativeUnavailable)?;
                waiting
                    .await
                    .map_err(|_| NativeFailure::NativeUnavailable)?
            })
        }
    }
    fn roundtrip(request: &ManagementRequest) -> Result<ManagementProgress, NativeFailure> {
        type Send = unsafe extern "C" fn(u32, *const u8, *mut u8, *mut usize, u32) -> u32;
        // SAFETY: this is the exact fixed native sender ABI; no path or library is opened.
        let send: Send = unsafe { linked_function(c"st_management_send_gui_demand")? };
        let mut bytes = [0; reply::REPLY_LIMIT];
        let mut length = 0;
        // SAFETY: exact bounded input/output buffers are initialized and live for the call.
        classify(unsafe {
            send(
                build_code(request.key().build_identity),
                request.encode().as_ptr(),
                bytes.as_mut_ptr(),
                &mut length,
                300_000,
            )
        })?;
        if length > bytes.len() {
            return Err(NativeFailure::InvalidRecord);
        }
        let progress = reply::decode(&bytes[..length])??;
        if progress.request != *request.request_bytes() {
            return Err(NativeFailure::IdentityMismatch);
        }
        Ok(progress)
    }
    fn build_code(build: BuildIdentity) -> u32 {
        match build {
            BuildIdentity::Release => 1,
            BuildIdentity::Preflight => 2,
            BuildIdentity::Development => 3,
        }
    }
    fn classify(code: u32) -> Result<(), NativeFailure> {
        if code == 0 {
            Ok(())
        } else {
            Err(NativeFailure::try_from(code)?)
        }
    }
}
#[cfg(target_os = "macos")]
pub use native::{MachDemandCaller, MachDemandCallerPeer, MachGuiManagement};
