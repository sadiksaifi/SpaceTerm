//! Request-bound administrator leases. Native observations enter only through injected OS boundaries.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use crate::native_deployment::{account::NativeAccountProvider, contracts::NativeFailure};
use crate::native_management::{
    controller::{ASSESSMENT_LIFETIME, AuthorizationRoute},
    helper_lifetime::HelperDeadline,
    request::ManagementRequest,
};

/// Native monotonic time, separate from a caller's wall clock or transport timestamps.
pub trait NativeMonotonicClock: Send + Sync {
    /// Reads time in the original assessment's boot-bound clock domain.
    fn now(&self) -> Result<Duration, NativeFailure>;
}
/// OS monotonic clock. The assessment owner independently retains the boot identity.
pub struct SystemMonotonicClock;
impl NativeMonotonicClock for SystemMonotonicClock {
    fn now(&self) -> Result<Duration, NativeFailure> {
        let mut time = std::mem::MaybeUninit::<libc::timespec>::uninit();
        // SAFETY: clock_gettime initializes the stack object on success and retains no pointer.
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, time.as_mut_ptr()) } != 0 {
            return Err(NativeFailure::AuthorizationUnavailable);
        }
        // SAFETY: the successful call initialized the whole object.
        let time = unsafe { time.assume_init() };
        if time.tv_sec < 0 || !(0..1_000_000_000).contains(&time.tv_nsec) {
            return Err(NativeFailure::AuthorizationUnavailable);
        }
        Ok(Duration::new(time.tv_sec as u64, time.tv_nsec as u32))
    }
}

/// Effective native authorization policy. A password-authenticated cache is not a standing rule.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeGrantPolicy {
    /// This exact administrative grant required password authentication.
    PasswordAuthenticated,
    /// A temporary grant follows earlier password authentication.
    PasswordAuthenticatedCache,
    /// The relevant policy grants privilege without authentication.
    StandingPasswordlessSudo,
    /// A polkit rule grants custody-relevant privilege without authentication.
    StandingPasswordlessPolkit,
}

/// Narrow OS authorization and protected original-assessment boundary.
///
/// Caller and grant handles retain audit/process/polkit identity and approved code through use.
/// An assessment comes from the installation owner's protected request scope, including its
/// original boot, exact request, principal, and issuance time. It is not a second authority journal.
/// Production implementations must not construct these handles from request fields or environment.
pub trait NativeAuthorizationBoundary: Send + Sync + 'static {
    /// Retained actual OS-authenticated invoking principal.
    type Caller: Send + Sync + 'static;
    /// Retained original protected assessment, not a decoded client reply.
    type Assessment: Send + Sync + 'static;
    /// Retained operation-specific OS right, bound to this assessment and caller.
    type Grant: Send + Sync + 'static;
    /// Authenticates the actual message sender or broker/audit principal without SUDO_UID/PKEXEC_UID.
    fn bind_caller(
        &self,
        route: AuthorizationRoute,
        deadline: HelperDeadline,
    ) -> Result<Self::Caller, NativeFailure>;
    /// Retains the exact protected original scope and verifies its principal and target account.
    fn retain_assessment(
        &self,
        caller: &Self::Caller,
        request: &ManagementRequest,
        deadline: HelperDeadline,
    ) -> Result<Self::Assessment, NativeFailure>;
    /// Original immutable request, reconstructed by the native owner.
    fn request<'a>(&self, assessment: &'a Self::Assessment) -> &'a ManagementRequest;
    /// Original assessment issuance, never authentication completion or helper entry.
    fn issued(&self, assessment: &Self::Assessment) -> Duration;
    /// Rechecks the retained actual sender, boot, and protected request scope without a grant.
    /// This grants only access to an already-admitted identical journal entry.
    fn recheck_caller(
        &self,
        caller: &Self::Caller,
        assessment: &Self::Assessment,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure>;
    /// Verifies the locally derived exact right without prompting in the privileged helper.
    fn verify_right(
        &self,
        caller: &Self::Caller,
        assessment: &Self::Assessment,
        route: AuthorizationRoute,
        deadline: HelperDeadline,
    ) -> Result<Self::Grant, NativeFailure>;
    /// Native observation of the exact granted build namespace and operation-specific right.
    fn granted_right(
        &self,
        grant: &Self::Grant,
    ) -> (
        crate::startup::BuildIdentity,
        crate::native_management::request::OperationTag,
    );
    /// Original immutable request to which the retained grant was enrolled by the native owner.
    fn granted_request<'a>(&self, grant: &'a Self::Grant) -> &'a ManagementRequest;
    /// Observes the effective supported rule, not broker exit status or a usable password cache.
    fn policy(&self, grant: &Self::Grant) -> Result<NativeGrantPolicy, NativeFailure>;
    /// Revalidates actual caller, native boot, original scope, and exact right through retained handles.
    fn recheck(
        &self,
        caller: &Self::Caller,
        assessment: &Self::Assessment,
        grant: &Self::Grant,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure>;
    /// Verifies a saved external form against the originally enrolled grant, not just the right name.
    fn recheck_external_form(
        &self,
        grant: &Self::Grant,
        external: &[u8; 32],
    ) -> Result<(), NativeFailure>;
    /// Atomically consumes the form in the original protected scope. A saved form cannot enroll another request.
    fn consume_external_form(
        &self,
        grant: &Self::Grant,
        external: &[u8; 32],
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure>;
    /// Destroys owned rights on cancellation, expiry, failed admission, or scope retirement.
    fn revoke(&self, grant: &mut Self::Grant);
}

trait LiveCaller: Send + Sync {
    fn request(&self) -> &ManagementRequest;
    fn recheck(&self, deadline: HelperDeadline) -> Result<(), NativeFailure>;
}

/// Authenticated lookup scope. It cannot authorize admission or a saved external form.
pub struct NativeCallerLease {
    deadline: HelperDeadline,
    native: Box<dyn LiveCaller>,
}
impl std::fmt::Debug for NativeCallerLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeCallerLease(..)")
    }
}
impl NativeCallerLease {
    /// Exact original comparison facts obtained from the protected native owner.
    pub fn request(&self) -> &ManagementRequest {
        self.native.request()
    }
    /// Revalidates native caller and current account incarnation, not assessment expiry.
    pub fn recheck(&self, deadline: HelperDeadline) -> Result<(), NativeFailure> {
        deadline.check()?;
        self.native.recheck(self.deadline)?;
        deadline.check()
    }
}
struct BoundCaller<P: NativeAuthorizationBoundary> {
    boundary: Arc<P>,
    accounts: Arc<dyn NativeAccountProvider>,
    caller: P::Caller,
    assessment: P::Assessment,
}
impl<P: NativeAuthorizationBoundary> LiveCaller for BoundCaller<P> {
    fn request(&self) -> &ManagementRequest {
        self.boundary.request(&self.assessment)
    }
    fn recheck(&self, deadline: HelperDeadline) -> Result<(), NativeFailure> {
        deadline.check()?;
        let request = self.request();
        let account = self.accounts.observe(request.key())?;
        if account.key() != request.key() || account.sha256() != *request.account_binding() {
            return Err(NativeFailure::IdentityMismatch);
        }
        self.boundary
            .recheck_caller(&self.caller, &self.assessment, deadline)?;
        deadline.check()
    }
}

trait LiveAuthorization: Send + Sync {
    fn request(&self) -> &ManagementRequest;
    fn recheck(&self, deadline: HelperDeadline) -> Result<(), NativeFailure>;
    fn recheck_external_form(&self, external: &[u8; 32]) -> Result<(), NativeFailure>;
    fn consume_external_form(
        &self,
        external: &[u8; 32],
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure>;
}

/// Noncloneable native lease. No constructor accepts request facts, a form, or an authorization boolean.
/// The coordinator's common AuthorizedNativeRequest wrapper must own this lease, not copy its facts.
pub struct NativeAuthorizationLease {
    deadline: HelperDeadline,
    native: Box<dyn LiveAuthorization>,
}
impl std::fmt::Debug for NativeAuthorizationLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeAuthorizationLease(..)")
    }
}
impl NativeAuthorizationLease {
    /// Exact original request independently reconstructed from protected native state.
    pub fn request(&self) -> &ManagementRequest {
        self.native.request()
    }
    /// Rechecks the original expiry, account incarnation, actual caller, and native right.
    pub fn recheck(&self, deadline: HelperDeadline) -> Result<(), NativeFailure> {
        deadline.check()?;
        self.recheck_original()?;
        deadline.check()
    }
    /// Rechecks with the original helper deadline retained by this lease.
    /// The common transaction authority can call this without constructing a new budget.
    pub fn recheck_original(&self) -> Result<(), NativeFailure> {
        self.native.recheck(self.deadline)
    }
    /// Rechecks a saved GUI form against the exact original grant under trusted native admission.
    pub fn recheck_external_form(
        &self,
        request: &ManagementRequest,
        external: &[u8; 32],
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        if request != self.request() {
            return Err(NativeFailure::IdentityMismatch);
        }
        self.recheck(deadline)?;
        self.native.recheck_external_form(external)?;
        self.recheck(deadline)
    }
    /// Retains a scoped Rust/C admission callback for this live lease and original helper deadline.
    pub fn gui_admission(
        &self,
        deadline: HelperDeadline,
    ) -> Result<GuiAdmissionScope<'_>, NativeFailure> {
        self.recheck(deadline)?;
        Ok(GuiAdmissionScope {
            lease: self,
            deadline: self.deadline,
        })
    }
}

struct BoundAuthorization<P: NativeAuthorizationBoundary> {
    boundary: Arc<P>,
    accounts: Arc<dyn NativeAccountProvider>,
    clock: Arc<dyn NativeMonotonicClock>,
    caller: P::Caller,
    assessment: P::Assessment,
    grant: Mutex<Option<P::Grant>>,
    route: AuthorizationRoute,
}
impl<P: NativeAuthorizationBoundary> BoundAuthorization<P> {
    fn with_grant<T>(
        &self,
        effect: impl FnOnce(&P::Grant) -> Result<T, NativeFailure>,
    ) -> Result<T, NativeFailure> {
        let mut grant = self
            .grant
            .lock()
            .map_err(|_| NativeFailure::AuthorizationUnavailable)?;
        let live = grant.as_ref().ok_or(NativeFailure::AuthorizationDenied)?;
        let result = effect(live);
        // Expired rights are revoked while the scope owner is still alive, not just on GUI release.
        if result.is_err()
            && let Some(mut owned) = grant.take()
        {
            self.boundary.revoke(&mut owned);
        }
        result
    }
}
impl<P: NativeAuthorizationBoundary> LiveAuthorization for BoundAuthorization<P> {
    fn request(&self) -> &ManagementRequest {
        self.boundary.request(&self.assessment)
    }
    fn recheck(&self, deadline: HelperDeadline) -> Result<(), NativeFailure> {
        self.with_grant(|grant| {
            deadline.check()?;
            check_expiry(self.clock.now()?, self.boundary.issued(&self.assessment))?;
            let request = self.request();
            let account = self.accounts.observe(request.key())?;
            if account.key() != request.key() || account.sha256() != *request.account_binding() {
                return Err(NativeFailure::IdentityMismatch);
            }
            if self.boundary.granted_right(grant)
                != (request.key().build_identity, request.operation())
                || self.boundary.granted_request(grant) != request
            {
                return Err(NativeFailure::AuthorizationDenied);
            }
            check_policy(self.boundary.policy(grant)?)?;
            self.boundary
                .recheck(&self.caller, &self.assessment, grant, deadline)?;
            check_expiry(self.clock.now()?, self.boundary.issued(&self.assessment))?;
            deadline.check()
        })
    }
    fn recheck_external_form(&self, external: &[u8; 32]) -> Result<(), NativeFailure> {
        self.with_grant(|grant| {
            if self.route != AuthorizationRoute::Gui || external == &[0; 32] {
                return Err(NativeFailure::AuthorizationDenied);
            }
            self.boundary.recheck_external_form(grant, external)
        })
    }
    fn consume_external_form(
        &self,
        external: &[u8; 32],
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        self.recheck(deadline)?;
        self.recheck_external_form(external)?;
        self.with_grant(|grant| {
            self.boundary
                .consume_external_form(grant, external, deadline)
        })
    }
}
impl<P: NativeAuthorizationBoundary> Drop for BoundAuthorization<P> {
    fn drop(&mut self) {
        if let Ok(grant) = self.grant.get_mut()
            && let Some(mut grant) = grant.take()
        {
            self.boundary.revoke(&mut grant);
        }
    }
}

/// Trusted issuer selected by native composition. Test peers are injected only in test composition.
pub struct NativeAuthorizationIssuer<P> {
    boundary: Arc<P>,
    accounts: Arc<dyn NativeAccountProvider>,
    clock: Arc<dyn NativeMonotonicClock>,
}
impl<P: NativeAuthorizationBoundary> NativeAuthorizationIssuer<P> {
    /// Injects OS peers, the seed account provider, and native time. It supplies no authority itself.
    pub fn new(
        boundary: Arc<P>,
        accounts: Arc<dyn NativeAccountProvider>,
        clock: Arc<dyn NativeMonotonicClock>,
    ) -> Self {
        Self {
            boundary,
            accounts,
            clock,
        }
    }
    /// Authenticates only retained-request lookup. No right or mutation lease is issued.
    pub fn authenticate(
        &self,
        request: &ManagementRequest,
        route: AuthorizationRoute,
        deadline: HelperDeadline,
    ) -> Result<NativeCallerLease, NativeFailure> {
        deadline.check()?;
        let caller = self.boundary.bind_caller(route, deadline)?;
        let assessment = self
            .boundary
            .retain_assessment(&caller, request, deadline)?;
        if self.boundary.request(&assessment) != request {
            return Err(NativeFailure::IdentityMismatch);
        }
        let lease = NativeCallerLease {
            deadline,
            native: Box::new(BoundCaller {
                boundary: Arc::clone(&self.boundary),
                accounts: Arc::clone(&self.accounts),
                caller,
                assessment,
            }),
        };
        lease.recheck(deadline)?;
        Ok(lease)
    }
    /// Independently authenticates one exact original request and returns a live nonserializable lease.
    pub fn authorize(
        &self,
        request: &ManagementRequest,
        route: AuthorizationRoute,
        deadline: HelperDeadline,
    ) -> Result<NativeAuthorizationLease, NativeFailure> {
        deadline.check()?;
        let caller = self.boundary.bind_caller(route, deadline)?;
        let assessment = self
            .boundary
            .retain_assessment(&caller, request, deadline)?;
        if self.boundary.request(&assessment) != request {
            return Err(NativeFailure::IdentityMismatch);
        }
        check_expiry(self.clock.now()?, self.boundary.issued(&assessment))?;
        let account = self.accounts.observe(request.key())?;
        if account.key() != request.key() || account.sha256() != *request.account_binding() {
            return Err(NativeFailure::IdentityMismatch);
        }
        deadline.check()?;
        let grant = self
            .boundary
            .verify_right(&caller, &assessment, route, deadline)?;
        let lease = NativeAuthorizationLease {
            deadline,
            native: Box::new(BoundAuthorization {
                boundary: Arc::clone(&self.boundary),
                accounts: Arc::clone(&self.accounts),
                clock: Arc::clone(&self.clock),
                caller,
                assessment,
                grant: Mutex::new(Some(grant)),
                route,
            }),
        };
        // This covers an authorization delay and revokes the grant on every rejected return.
        lease.recheck(deadline)?;
        Ok(lease)
    }
}

fn check_expiry(now: Duration, issued: Duration) -> Result<(), NativeFailure> {
    let age = now
        .checked_sub(issued)
        .ok_or(NativeFailure::AuthorizationUnavailable)?;
    if age >= ASSESSMENT_LIFETIME {
        return Err(NativeFailure::StaleAssessment);
    }
    Ok(())
}
fn check_policy(policy: NativeGrantPolicy) -> Result<(), NativeFailure> {
    match policy {
        NativeGrantPolicy::PasswordAuthenticated
        | NativeGrantPolicy::PasswordAuthenticatedCache => Ok(()),
        NativeGrantPolicy::StandingPasswordlessSudo => Err(NativeFailure::LinuxPasswordlessSudo),
        NativeGrantPolicy::StandingPasswordlessPolkit => {
            Err(NativeFailure::LinuxUnsupportedAuthorizationPolicy)
        }
    }
}

/// Borrowed trusted callback scope. It contains no decodable native authority and cannot outlive its lease.
pub struct GuiAdmissionScope<'a> {
    lease: &'a NativeAuthorizationLease,
    deadline: HelperDeadline,
}
impl GuiAdmissionScope<'_> {
    /// In-process opaque pointer for st_management_verify_gui_right_bound, never a transport value.
    pub fn as_native_handle(&self) -> *const std::ffi::c_void {
        std::ptr::from_ref(self).cast()
    }
}

/// Rechecks an opaque native scope before authd inspection.
///
/// # Safety
/// `scope` must point to a live GuiAdmissionScope issued by this process. The two byte pointers
/// must refer to initialized arrays of 160 and 32 bytes. None may be supplied as a remote pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn st_management_admission_recheck(
    scope: *const std::ffi::c_void,
    request: *const u8,
    external: *const u8,
) -> u32 {
    // SAFETY: the caller's native lifetime and buffer contract is propagated unchanged.
    unsafe { with_native_scope(scope, request, external, false) }
}
/// Atomically consumes an external form under its original protected scope.
///
/// # Safety
/// The same trusted in-process pointer and exact-buffer contract as st_management_admission_recheck applies.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn st_management_admission_consume(
    scope: *const std::ffi::c_void,
    request: *const u8,
    external: *const u8,
) -> u32 {
    // SAFETY: the caller's native lifetime and buffer contract is propagated unchanged.
    unsafe { with_native_scope(scope, request, external, true) }
}
unsafe fn with_native_scope(
    scope: *const std::ffi::c_void,
    request: *const u8,
    external: *const u8,
    consume: bool,
) -> u32 {
    if scope.is_null() || request.is_null() || external.is_null() {
        return NativeFailure::InvalidRecord as u32;
    }
    // SAFETY: only the trusted native boundary passes this issuer-owned scope and exact arrays.
    let (scope, bytes, external) = unsafe {
        (
            &*scope.cast::<GuiAdmissionScope<'_>>(),
            std::slice::from_raw_parts(request, 160),
            &*external.cast::<[u8; 32]>(),
        )
    };
    let result = ManagementRequest::decode(bytes).and_then(|request| {
        scope
            .lease
            .recheck_external_form(&request, external, scope.deadline)?;
        if consume {
            scope
                .lease
                .native
                .consume_external_form(external, scope.deadline)?;
        }
        Ok(())
    });
    match result {
        Ok(()) => 0,
        Err(failure) => failure as u32,
    }
}
