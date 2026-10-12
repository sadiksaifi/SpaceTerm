use super::*;
use native_management::facade::{
    authorization::{
        NativeAuthorizationBoundary, NativeAuthorizationIssuer, NativeAuthorizationLease,
        NativeCallerLease, NativeGrantPolicy, NativeMonotonicClock,
        st_management_admission_consume, st_management_admission_recheck,
    },
    backend::{NativeManagerBackend, NativeRetainedRequestLookup},
    entry::{ManagerInvocation, dispatch},
};
use spaceterm_host::native_deployment::{
    account::NativeAccountProvider,
    contracts::{AccountBinding, NativeAccountId},
};
use std::time::Duration;

struct Clock(Mutex<Duration>);
impl NativeMonotonicClock for Clock {
    fn now(&self) -> Result<Duration, NativeFailure> {
        Ok(*self.0.lock().unwrap())
    }
}
struct Accounts(Mutex<AccountBinding>);
impl NativeAccountProvider for Accounts {
    fn observe(
        &self,
        _: spaceterm_host::startup::AccountBuildKey,
    ) -> Result<AccountBinding, NativeFailure> {
        Ok(self.0.lock().unwrap().clone())
    }
}
struct Caller(u32);
struct OriginalAssessment {
    request: ManagementRequest,
    issued: Duration,
}
struct Grant {
    request: ManagementRequest,
    operation: OperationTag,
    consumed: Mutex<bool>,
}
struct Peer {
    original: ManagementRequest,
    clock: Arc<Clock>,
    issued: Duration,
    policy: NativeGrantPolicy,
    actual_caller: u32,
    wrong_right: bool,
    failure: Option<NativeFailure>,
    authorization_delay: Duration,
    observations: Mutex<(u32, u32, u32)>,
}
impl NativeAuthorizationBoundary for Peer {
    type Caller = Caller;
    type Assessment = OriginalAssessment;
    type Grant = Grant;
    fn bind_caller(
        &self,
        _: AuthorizationRoute,
        _: HelperDeadline,
    ) -> Result<Caller, NativeFailure> {
        self.observations.lock().unwrap().0 += 1;
        if let Some(failure) = self.failure {
            return Err(failure);
        }
        Ok(Caller(self.actual_caller))
    }
    fn retain_assessment(
        &self,
        caller: &Caller,
        request: &ManagementRequest,
        _: HelperDeadline,
    ) -> Result<OriginalAssessment, NativeFailure> {
        // This fake stands in for native authenticated caller plus protected original scope.
        if caller.0 != 501 || request != &self.original {
            return Err(NativeFailure::IdentityMismatch);
        }
        Ok(OriginalAssessment {
            request: self.original.clone(),
            issued: self.issued,
        })
    }
    fn request<'a>(&self, assessment: &'a OriginalAssessment) -> &'a ManagementRequest {
        &assessment.request
    }
    fn issued(&self, assessment: &OriginalAssessment) -> Duration {
        assessment.issued
    }
    fn recheck_caller(
        &self,
        caller: &Caller,
        assessment: &OriginalAssessment,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        deadline.check()?;
        if caller.0 != self.actual_caller || caller.0 != 501 || assessment.request != self.original
        {
            return Err(NativeFailure::IdentityMismatch);
        }
        Ok(())
    }
    fn verify_right(
        &self,
        _: &Caller,
        _: &OriginalAssessment,
        _: AuthorizationRoute,
        _: HelperDeadline,
    ) -> Result<Grant, NativeFailure> {
        *self.clock.0.lock().unwrap() += self.authorization_delay;
        self.observations.lock().unwrap().1 += 1;
        Ok(Grant {
            request: self.original.clone(),
            operation: if self.wrong_right {
                OperationTag::Stop
            } else {
                self.original.operation()
            },
            consumed: Mutex::new(false),
        })
    }
    fn granted_right(&self, grant: &Grant) -> (BuildIdentity, OperationTag) {
        (grant.request.key().build_identity, grant.operation)
    }
    fn granted_request<'a>(&self, grant: &'a Grant) -> &'a ManagementRequest {
        &grant.request
    }
    fn policy(&self, _: &Grant) -> Result<NativeGrantPolicy, NativeFailure> {
        Ok(self.policy)
    }
    fn recheck(
        &self,
        _: &Caller,
        _: &OriginalAssessment,
        _: &Grant,
        _: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        Ok(())
    }
    fn recheck_external_form(
        &self,
        grant: &Grant,
        external: &[u8; 32],
    ) -> Result<(), NativeFailure> {
        if external != &[7; 32] || *grant.consumed.lock().unwrap() {
            return Err(NativeFailure::AuthorizationDenied);
        }
        Ok(())
    }
    fn consume_external_form(
        &self,
        grant: &Grant,
        external: &[u8; 32],
        _: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        let mut consumed = grant.consumed.lock().unwrap();
        if external != &[7; 32] || *consumed {
            return Err(NativeFailure::AuthorizationDenied);
        }
        *consumed = true;
        Ok(())
    }
    fn revoke(&self, _: &mut Grant) {
        self.observations.lock().unwrap().2 += 1;
    }
}
fn account(incarnation: u8) -> AccountBinding {
    AccountBinding::new(
        spaceterm_host::startup::AccountBuildKey {
            account_uid: 501,
            build_identity: BuildIdentity::Development,
        },
        20,
        NativeAccountId::macos([incarnation; 16]).unwrap(),
    )
    .unwrap()
}
fn setup() -> (Peer, Arc<Accounts>, Arc<Clock>) {
    let account = account(1);
    let mut bytes = frame(OperationTag::Start);
    bytes[56..88].copy_from_slice(&account.sha256());
    let clock = Arc::new(Clock(Mutex::new(Duration::from_secs(5))));
    let peer = Peer {
        original: ManagementRequest::decode(&bytes).unwrap(),
        clock: clock.clone(),
        issued: Duration::from_secs(1),
        actual_caller: 501,
        wrong_right: false,
        failure: None,
        authorization_delay: Duration::ZERO,
        policy: NativeGrantPolicy::PasswordAuthenticated,
        observations: Mutex::new((0, 0, 0)),
    };
    (peer, Arc::new(Accounts(Mutex::new(account))), clock)
}
fn issuer(
    peer: Arc<Peer>,
    accounts: Arc<Accounts>,
    clock: Arc<Clock>,
) -> NativeAuthorizationIssuer<Peer> {
    NativeAuthorizationIssuer::new(peer, accounts, clock)
}
#[test]
fn saved_unconsumed_external_form_is_rejected_after_original_expiry_with_owner_alive() {
    let (peer, accounts, clock) = setup();
    let peer = Arc::new(peer);
    let guard = native_management::helper_lifetime::HelperLifetimeGuard::install().unwrap();
    let authority = issuer(peer.clone(), accounts, clock.clone())
        .authorize(&peer.original, AuthorizationRoute::Gui, guard.deadline())
        .unwrap();
    let scope = authority.gui_admission(guard.deadline()).unwrap();
    let bytes = peer.original.encode();
    let form = [7u8; 32];
    *clock.0.lock().unwrap() = Duration::from_secs(31);
    // SAFETY: a live issuer-owned scope and exact initialized native arrays are retained for this call.
    let code = unsafe {
        st_management_admission_recheck(scope.as_native_handle(), bytes.as_ptr(), form.as_ptr())
    };
    assert_eq!(code, NativeFailure::StaleAssessment as u32);
    assert_eq!(*peer.observations.lock().unwrap(), (1, 1, 1));
    drop(authority);
    assert_eq!(peer.observations.lock().unwrap().2, 1);
}
#[test]
fn saved_form_cannot_authorize_request_substitution_or_another_form() {
    let (peer, accounts, clock) = setup();
    let peer = Arc::new(peer);
    let guard = native_management::helper_lifetime::HelperLifetimeGuard::install().unwrap();
    let authority = issuer(peer.clone(), accounts, clock)
        .authorize(&peer.original, AuthorizationRoute::Gui, guard.deadline())
        .unwrap();
    let mut substituted = peer.original.encode();
    substituted[88] ^= 1;
    let request = ManagementRequest::decode(&substituted).unwrap();
    assert_eq!(
        authority.recheck_external_form(&request, &[7; 32], guard.deadline()),
        Err(NativeFailure::IdentityMismatch)
    );
    assert_eq!(
        authority.recheck_external_form(&peer.original, &[8; 32], guard.deadline()),
        Err(NativeFailure::AuthorizationDenied)
    );
}
#[test]
fn native_admission_consumes_the_exact_grant_once() {
    let (peer, accounts, clock) = setup();
    let peer = Arc::new(peer);
    let guard = native_management::helper_lifetime::HelperLifetimeGuard::install().unwrap();
    let authority = issuer(peer.clone(), accounts, clock)
        .authorize(&peer.original, AuthorizationRoute::Gui, guard.deadline())
        .unwrap();
    let scope = authority.gui_admission(guard.deadline()).unwrap();
    let bytes = peer.original.encode();
    let form = [7u8; 32];
    // SAFETY: both invocations borrow the live native scope and initialized exact-size arrays.
    let first = unsafe {
        st_management_admission_consume(scope.as_native_handle(), bytes.as_ptr(), form.as_ptr())
    };
    let second = unsafe {
        st_management_admission_consume(scope.as_native_handle(), bytes.as_ptr(), form.as_ptr())
    };
    assert_eq!(
        (first, second),
        (0, NativeFailure::AuthorizationDenied as u32)
    );
}
#[test]
fn native_authorization_delay_never_restarts_original_assessment() {
    let (mut peer, accounts, clock) = setup();
    peer.authorization_delay = Duration::from_secs(27);
    let peer = Arc::new(peer);
    let guard = native_management::helper_lifetime::HelperLifetimeGuard::install().unwrap();
    let outcome = issuer(peer.clone(), accounts, clock).authorize(
        &peer.original,
        AuthorizationRoute::Gui,
        guard.deadline(),
    );
    assert!(matches!(outcome, Err(NativeFailure::StaleAssessment)));
    assert_eq!(*peer.observations.lock().unwrap(), (1, 1, 1));
}
#[test]
fn native_caller_not_request_uid_and_account_incarnation_are_revalidated() {
    let (mut peer, accounts, clock) = setup();
    peer.actual_caller = 502;
    let peer = Arc::new(peer);
    let guard = native_management::helper_lifetime::HelperLifetimeGuard::install().unwrap();
    let result = issuer(peer.clone(), accounts.clone(), clock.clone()).authorize(
        &peer.original,
        AuthorizationRoute::CliInteractive,
        guard.deadline(),
    );
    assert!(matches!(result, Err(NativeFailure::IdentityMismatch)));
    assert_eq!(peer.observations.lock().unwrap().1, 0);
    let (peer, _, _) = setup();
    let peer = Arc::new(peer);
    let authority = issuer(peer.clone(), accounts.clone(), clock)
        .authorize(
            &peer.original,
            AuthorizationRoute::CliInteractive,
            guard.deadline(),
        )
        .unwrap();
    *accounts.0.lock().unwrap() = account(2);
    assert_eq!(
        authority.recheck(guard.deadline()),
        Err(NativeFailure::IdentityMismatch)
    );
    assert_eq!(peer.observations.lock().unwrap().2, 1);
}

struct Owner(TestBackend);
impl NativeRetainedRequestLookup for Owner {
    fn lookup_authenticated(
        &self,
        request: &ManagementRequest,
        caller: &NativeCallerLease,
        deadline: HelperDeadline,
    ) -> Result<Option<JournalAdmission<Self::Transaction>>, NativeFailure> {
        caller.recheck(deadline)?;
        if caller.request() != request {
            return Err(NativeFailure::IdentityMismatch);
        }
        self.0.lookup_existing(request, &TestAuthority, deadline)
    }
}
impl ManagementBackend for Owner {
    type Authorization = NativeAuthorizationLease;
    type Transaction = TestTransaction;
    fn authorize(
        &self,
        _: &ManagementRequest,
        _: AuthorizationRoute,
        _: HelperDeadline,
    ) -> Result<Self::Authorization, NativeFailure> {
        panic!("the native issuer, not the journal owner, authenticates the caller")
    }
    fn lookup_existing(
        &self,
        request: &ManagementRequest,
        _: &Self::Authorization,
        deadline: HelperDeadline,
    ) -> Result<Option<JournalAdmission<Self::Transaction>>, NativeFailure> {
        self.0.lookup_existing(request, &TestAuthority, deadline)
    }
    fn revalidate(
        &self,
        request: &ManagementRequest,
        authority: &Self::Authorization,
        deadline: HelperDeadline,
    ) -> Result<Duration, NativeFailure> {
        authority.recheck(deadline)?;
        self.0.revalidate(request, &TestAuthority, deadline)
    }
    fn admit(
        &self,
        request: &ManagementRequest,
        authority: Self::Authorization,
        deadline: HelperDeadline,
    ) -> Result<JournalAdmission<Self::Transaction>, NativeFailure> {
        authority.recheck(deadline)?;
        self.0.admit(request, TestAuthority, deadline)
    }
    fn resolve(
        &self,
        request: [u8; 16],
        deadline: HelperDeadline,
    ) -> Result<JournalAdmission<Self::Transaction>, NativeFailure> {
        self.0.resolve(request, deadline)
    }
    fn request<'a>(&self, t: &'a Self::Transaction) -> &'a ManagementRequest {
        self.0.request(t)
    }
    fn phase(&self, t: &Self::Transaction) -> ManagementPhase {
        self.0.phase(t)
    }
    fn desired_state(
        &self,
        t: &Self::Transaction,
        d: HelperDeadline,
    ) -> Result<DesiredServerState, NativeFailure> {
        self.0.desired_state(t, d)
    }
    fn commit_stopped(&self, t: &mut Self::Transaction, d: HelperDeadline) -> IntentDisposition {
        self.0.commit_stopped(t, d)
    }
    fn suspend_automatic_start(
        &self,
        t: &mut Self::Transaction,
        d: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        self.0.suspend_automatic_start(t, d)
    }
    fn begin_quiescence(
        &self,
        t: &mut Self::Transaction,
        d: HelperDeadline,
    ) -> Result<Quiescence, NativeFailure> {
        self.0.begin_quiescence(t, d)
    }
    fn poll_quiescence(
        &self,
        t: &mut Self::Transaction,
        d: HelperDeadline,
    ) -> Result<Quiescence, NativeFailure> {
        self.0.poll_quiescence(t, d)
    }
    fn retire_supervision(
        &self,
        t: &mut Self::Transaction,
        d: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        self.0.retire_supervision(t, d)
    }
    fn finish_stop(
        &self,
        t: &mut Self::Transaction,
        d: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        self.0.finish_stop(t, d)
    }
    fn start_approved(
        &self,
        t: &mut Self::Transaction,
        d: HelperDeadline,
    ) -> Result<ManagementProgress, NativeFailure> {
        self.0.start_approved(t, d)
    }
    fn restart_approved(
        &self,
        t: &mut Self::Transaction,
        d: HelperDeadline,
    ) -> Result<ManagementProgress, NativeFailure> {
        self.0.restart_approved(t, d)
    }
    fn apply_owned_operation(
        &self,
        t: &mut Self::Transaction,
        d: HelperDeadline,
    ) -> Result<ManagementProgress, NativeFailure> {
        d.check()?;
        if t.request.operation() == OperationTag::Install {
            return Ok(self.0.finish(t, "installation-owner-applied"));
        }
        self.0.apply_owned_operation(t, d)
    }
}
#[test]
fn accepted_requests_reach_actual_controller_through_injected_native_peers() {
    let (peer, accounts, clock) = setup();
    let peer = Arc::new(peer);
    let owner = TestBackend::new();
    let backend =
        NativeManagerBackend::new(issuer(peer.clone(), accounts, clock), Owner(owner.clone()));
    let outcome = controller(backend)
        .execute(&peer.original, AuthorizationRoute::Gui)
        .unwrap();
    assert_eq!(outcome.state, ProgressState::Complete);
    assert_eq!(owner.0.lock().unwrap().started, [42]);
    assert_eq!(peer.observations.lock().unwrap().2, 1);
}
#[test]
fn denied_cancelled_unavailable_and_stale_paths_have_zero_journal_or_runtime_effects() {
    for failure in [
        NativeFailure::AuthorizationDenied,
        NativeFailure::AuthorizationCancelled,
        NativeFailure::AuthorizationUnavailable,
    ] {
        let (mut peer, accounts, clock) = setup();
        peer.failure = Some(failure);
        let peer = Arc::new(peer);
        let owner = TestBackend::new();
        let backend =
            NativeManagerBackend::new(issuer(peer.clone(), accounts, clock), Owner(owner.clone()));
        assert_eq!(
            controller(backend).execute(&peer.original, AuthorizationRoute::Gui),
            Err(failure)
        );
        assert!(owner.0.lock().unwrap().events.is_empty());
    }
    let (peer, accounts, clock) = setup();
    *clock.0.lock().unwrap() = Duration::from_secs(31);
    let peer = Arc::new(peer);
    let owner = TestBackend::new();
    let backend =
        NativeManagerBackend::new(issuer(peer.clone(), accounts, clock), Owner(owner.clone()));
    assert_eq!(
        controller(backend).execute(&peer.original, AuthorizationRoute::Gui),
        Err(NativeFailure::StaleAssessment)
    );
    assert!(owner.0.lock().unwrap().events.is_empty());
}
#[test]
fn password_authenticated_caches_are_accepted_but_standing_passwordless_rules_are_refused() {
    for route in [
        AuthorizationRoute::Gui,
        AuthorizationRoute::CliInteractive,
        AuthorizationRoute::CliNonInteractive,
    ] {
        let (mut peer, accounts, clock) = setup();
        peer.policy = NativeGrantPolicy::PasswordAuthenticatedCache;
        let peer = Arc::new(peer);
        let owner = TestBackend::new();
        let backend =
            NativeManagerBackend::new(issuer(peer.clone(), accounts, clock), Owner(owner.clone()));
        assert_eq!(
            controller(backend)
                .execute(&peer.original, route)
                .unwrap()
                .state,
            ProgressState::Complete
        );
        let (mut peer, accounts, clock) = setup();
        peer.policy = match route {
            AuthorizationRoute::Gui => NativeGrantPolicy::StandingPasswordlessPolkit,
            _ => NativeGrantPolicy::StandingPasswordlessSudo,
        };
        let peer = Arc::new(peer);
        let owner = TestBackend::new();
        let backend =
            NativeManagerBackend::new(issuer(peer.clone(), accounts, clock), Owner(owner.clone()));
        assert!(controller(backend).execute(&peer.original, route).is_err());
        assert!(owner.0.lock().unwrap().events.is_empty());
    }
}
#[test]
fn pkexec_operation_substitution_is_rejected_before_authentication_or_effects() {
    let (peer, accounts, clock) = setup();
    let peer = Arc::new(peer);
    let owner = TestBackend::new();
    let backend =
        NativeManagerBackend::new(issuer(peer.clone(), accounts, clock), Owner(owner.clone()));
    let guard = native_management::helper_lifetime::HelperLifetimeGuard::install().unwrap();
    dispatch(
        backend,
        guard,
        ManagerInvocation::Polkit(OperationTag::Stop),
        peer.original.clone(),
        |result, _| {
            assert_eq!(result, Err(NativeFailure::IdentityMismatch));
        },
    );
    assert_eq!(peer.observations.lock().unwrap().0, 0);
    assert!(owner.0.lock().unwrap().events.is_empty());
}

#[test]
fn a_grant_for_another_operation_cannot_reach_controller_effects() {
    let (mut peer, accounts, clock) = setup();
    peer.wrong_right = true;
    let peer = Arc::new(peer);
    let owner = TestBackend::new();
    let backend =
        NativeManagerBackend::new(issuer(peer.clone(), accounts, clock), Owner(owner.clone()));
    assert_eq!(
        controller(backend).execute(&peer.original, AuthorizationRoute::Gui),
        Err(NativeFailure::AuthorizationDenied)
    );
    assert!(owner.0.lock().unwrap().events.is_empty());
    assert_eq!(peer.observations.lock().unwrap().2, 1);
}

#[test]
fn a_new_helper_guard_cannot_extend_an_issued_native_lease() {
    let (peer, accounts, clock) = setup();
    let peer = Arc::new(peer);
    let original = native_management::helper_lifetime::HelperLifetimeGuard::install_for(
        Duration::from_secs(2),
    )
    .unwrap();
    let lease = issuer(peer.clone(), accounts, clock)
        .authorize(&peer.original, AuthorizationRoute::Gui, original.deadline())
        .unwrap();
    // The test retires its watchdog thread, not a production manager or a native grant.
    drop(original);
    std::thread::sleep(Duration::from_secs(2) + Duration::from_millis(25));
    let another = native_management::helper_lifetime::HelperLifetimeGuard::install().unwrap();
    assert_eq!(
        lease.recheck(another.deadline()),
        Err(NativeFailure::TimedOut)
    );
}

#[test]
fn native_backend_exact_retry_after_original_expiry_returns_retained_outcome_without_effects() {
    let (peer, accounts, clock) = setup();
    let peer = Arc::new(peer);
    let owner = TestBackend::new();
    let run = || {
        controller(NativeManagerBackend::new(
            issuer(peer.clone(), accounts.clone(), clock.clone()),
            Owner(owner.clone()),
        ))
    };
    let first = run()
        .execute(&peer.original, AuthorizationRoute::Gui)
        .unwrap();
    let effects = owner.0.lock().unwrap().events.clone();
    *clock.0.lock().unwrap() = Duration::from_secs(31);
    assert_eq!(
        run().execute(&peer.original, AuthorizationRoute::Gui),
        Ok(first)
    );
    assert_eq!(owner.0.lock().unwrap().events, effects);
    assert_eq!(owner.0.lock().unwrap().started, [42]);
    let mut bytes = peer.original.encode();
    bytes[88] ^= 1;
    assert_eq!(
        run().execute(
            &ManagementRequest::decode(&bytes).unwrap(),
            AuthorizationRoute::Gui
        ),
        Err(NativeFailure::IdentityMismatch)
    );
    *accounts.0.lock().unwrap() = account(2);
    assert_eq!(
        run().execute(&peer.original, AuthorizationRoute::Gui),
        Err(NativeFailure::IdentityMismatch)
    );
    assert_eq!(owner.0.lock().unwrap().events, effects);
}

#[test]
fn native_backend_expired_pending_retry_resumes_only_the_original_admitted_operation() {
    let (mut peer, accounts, clock) = setup();
    let mut bytes = peer.original.encode();
    bytes[5] = OperationTag::Stop as u8;
    peer.original = ManagementRequest::decode(&bytes).unwrap();
    let peer = Arc::new(peer);
    let owner = TestBackend::new();
    owner.0.lock().unwrap().drained = false;
    let run = || {
        controller(NativeManagerBackend::new(
            issuer(peer.clone(), accounts.clone(), clock.clone()),
            Owner(owner.clone()),
        ))
    };
    assert_eq!(
        run()
            .execute(&peer.original, AuthorizationRoute::Gui)
            .unwrap()
            .state,
        ProgressState::Pending
    );
    *clock.0.lock().unwrap() = Duration::from_secs(31);
    assert_eq!(
        run()
            .execute(&peer.original, AuthorizationRoute::Gui)
            .unwrap()
            .state,
        ProgressState::Pending
    );
    owner.0.lock().unwrap().drained = true;
    assert_eq!(
        run()
            .execute(&peer.original, AuthorizationRoute::Gui)
            .unwrap()
            .state,
        ProgressState::Complete
    );
    assert_eq!(
        owner
            .0
            .lock()
            .unwrap()
            .events
            .iter()
            .filter(|event| **event == "journal-admitted")
            .count(),
        1
    );
    assert_eq!(peer.observations.lock().unwrap().1, 1);
}

#[test]
fn gui_demand_dispatch_reaches_controller_after_native_caller_authentication() {
    let (peer, accounts, clock) = setup();
    let peer = Arc::new(peer);
    let owner = TestBackend::new();
    let backend =
        NativeManagerBackend::new(issuer(peer.clone(), accounts, clock), Owner(owner.clone()));
    dispatch(
        backend,
        native_management::helper_lifetime::HelperLifetimeGuard::install().unwrap(),
        ManagerInvocation::GuiDemand,
        peer.original.clone(),
        |result, _| assert_eq!(result.unwrap().state, ProgressState::Complete),
    );
    assert_eq!(owner.0.lock().unwrap().started, [42]);
}

use native_management::facade::transaction::{
    ManagerJournal, ManagerTransactionPeer, TransactionBackedManager,
};
struct TransactionPeer {
    calls: Arc<Mutex<u32>>,
    accounts: Arc<Accounts>,
    change_account_under_lock: bool,
}
impl ManagerTransactionPeer for TransactionPeer {
    type Lease = NativeAuthorizationLease;
    fn begin(
        &self,
        lease: Self::Lease,
        deadline: HelperDeadline,
    ) -> Result<Self::Lease, NativeFailure> {
        lease.recheck(deadline)?;
        *self.calls.lock().unwrap() += 1;
        if self.change_account_under_lock {
            *self.accounts.0.lock().unwrap() = account(2);
        }
        Ok(lease)
    }
    fn recheck(&self, lease: &Self::Lease, deadline: HelperDeadline) -> Result<(), NativeFailure> {
        lease.recheck(deadline)
    }
    fn request<'a>(&self, lease: &'a Self::Lease) -> &'a ManagementRequest {
        lease.request()
    }
}
impl ManagerJournal<NativeAuthorizationLease> for Owner {
    fn lookup_authenticated(
        &self,
        request: &ManagementRequest,
        caller: &NativeCallerLease,
        deadline: HelperDeadline,
    ) -> Result<Option<JournalAdmission<Self::Transaction>>, NativeFailure> {
        NativeRetainedRequestLookup::lookup_authenticated(self, request, caller, deadline)
    }
    fn lookup_authorized(
        &self,
        request: &ManagementRequest,
        lease: &NativeAuthorizationLease,
        deadline: HelperDeadline,
    ) -> Result<Option<JournalAdmission<Self::Transaction>>, NativeFailure> {
        self.lookup_existing(request, lease, deadline)
    }
    fn revalidate_original(
        &self,
        request: &ManagementRequest,
        lease: &NativeAuthorizationLease,
        deadline: HelperDeadline,
    ) -> Result<Duration, NativeFailure> {
        self.revalidate(request, lease, deadline)
    }
}
#[test]
fn manager_transaction_adapter_admits_under_native_authority_and_rechecks_after_lock() {
    for change_account_under_lock in [false, true] {
        let (peer, accounts, clock) = setup();
        let peer = Arc::new(peer);
        let calls = Arc::new(Mutex::new(0));
        let owner = TestBackend::new();
        let native_transaction = TransactionPeer {
            calls: calls.clone(),
            accounts: accounts.clone(),
            change_account_under_lock,
        };
        let backend = NativeManagerBackend::new(
            issuer(peer.clone(), accounts, clock),
            TransactionBackedManager::new(native_transaction, Owner(owner.clone())),
        );
        let result = controller(backend).execute(&peer.original, AuthorizationRoute::Gui);
        assert_eq!(*calls.lock().unwrap(), 1);
        if change_account_under_lock {
            assert_eq!(result, Err(NativeFailure::IdentityMismatch));
            assert!(owner.0.lock().unwrap().events.is_empty());
        } else {
            assert_eq!(result.unwrap().state, ProgressState::Complete);
            assert_eq!(owner.0.lock().unwrap().started, [42]);
        }
    }
}

use native_management::facade::caller::{CallerBoundAuthorization, NativeCallerAuthentication};
struct AuditPeer(u32);
impl NativeCallerAuthentication for AuditPeer {
    type Caller = Caller;
    fn bind(&self, route: AuthorizationRoute, _: HelperDeadline) -> Result<Caller, NativeFailure> {
        assert_eq!(route, AuthorizationRoute::Gui);
        Ok(Caller(self.0))
    }
    fn uid(&self, caller: &Caller) -> Result<u32, NativeFailure> {
        Ok(caller.0)
    }
    fn recheck(&self, caller: &Caller, deadline: HelperDeadline) -> Result<(), NativeFailure> {
        deadline.check()?;
        if caller.0 != self.0 {
            return Err(NativeFailure::IdentityMismatch);
        }
        Ok(())
    }
}
#[test]
fn gui_demand_helper_binds_mach_audit_token_to_original_scope() {
    for uid in [501, 502] {
        let (scope, accounts, clock) = setup();
        let request = scope.original.clone();
        let boundary = Arc::new(CallerBoundAuthorization::new(AuditPeer(uid), scope));
        let issuer = NativeAuthorizationIssuer::new(boundary, accounts, clock);
        let guard = native_management::helper_lifetime::HelperLifetimeGuard::install().unwrap();
        let outcome = issuer.authorize(&request, AuthorizationRoute::Gui, guard.deadline());
        if uid == 501 {
            outcome.unwrap().recheck(guard.deadline()).unwrap();
        } else {
            assert!(matches!(outcome, Err(NativeFailure::IdentityMismatch)));
        }
    }
}

use native_management::linux::polkit::{
    NativePolkitPeer, PolkitAuthorization, PolkitNativeOutcome,
};
struct PolkitPeer {
    outcome: u8,
    uid: u32,
    calls: Arc<Mutex<Vec<&'static str>>>,
}
struct PolkitGrant(ManagementRequest);
impl NativePolkitPeer for PolkitPeer {
    type Subject = Caller;
    type Grant = PolkitGrant;
    fn subject(&self, _: HelperDeadline) -> Result<Caller, NativeFailure> {
        self.calls.lock().unwrap().push("subject");
        Ok(Caller(self.uid))
    }
    fn uid(&self, subject: &Caller) -> Result<u32, NativeFailure> {
        Ok(subject.0)
    }
    fn recheck_subject(
        &self,
        subject: &Caller,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        deadline.check()?;
        if subject.0 != self.uid {
            return Err(NativeFailure::IdentityMismatch);
        }
        Ok(())
    }
    fn authorize(
        &self,
        subject: &Caller,
        request: &ManagementRequest,
        action: &str,
        _: HelperDeadline,
    ) -> PolkitNativeOutcome<Self::Grant> {
        assert_eq!(subject.0, 501);
        assert_eq!(
            action,
            "io.github.sadiksaifi.spaceterm-development.server.manage.start"
        );
        self.calls.lock().unwrap().push("action");
        match self.outcome {
            0 => PolkitNativeOutcome::Granted(PolkitGrant(request.clone())),
            1 => PolkitNativeOutcome::Denied,
            2 => PolkitNativeOutcome::Cancelled,
            3 => PolkitNativeOutcome::AgentUnavailable,
            _ => PolkitNativeOutcome::Unavailable,
        }
    }
    fn policy(&self, _: &PolkitGrant) -> Result<NativeGrantPolicy, NativeFailure> {
        Ok(NativeGrantPolicy::PasswordAuthenticatedCache)
    }
    fn recheck_grant(
        &self,
        _: &Caller,
        grant: &PolkitGrant,
        request: &ManagementRequest,
        action: &str,
        _: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        if &grant.0 != request
            || action != "io.github.sadiksaifi.spaceterm-development.server.manage.start"
        {
            return Err(NativeFailure::IdentityMismatch);
        }
        Ok(())
    }
    fn revoke(&self, _: &mut PolkitGrant) {
        self.calls.lock().unwrap().push("revoke");
    }
}
#[test]
fn polkit_pkexec_uses_authenticated_subject_and_typed_native_outcome() {
    for outcome in 0..5 {
        let (scope, accounts, clock) = setup();
        let request = scope.original.clone();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let peer = PolkitPeer {
            outcome,
            uid: 501,
            calls: calls.clone(),
        };
        let issuer = NativeAuthorizationIssuer::new(
            Arc::new(PolkitAuthorization::new(peer, scope)),
            accounts,
            clock,
        );
        let guard = native_management::helper_lifetime::HelperLifetimeGuard::install().unwrap();
        let result =
            native_management::linux::authorize_native_request(&issuer, &request, guard.deadline());
        if outcome == 0 {
            let lease = result.unwrap();
            lease.recheck(guard.deadline()).unwrap();
            assert_eq!(
                lease.recheck_external_form(&request, &[7; 32], guard.deadline()),
                Err(NativeFailure::AuthorizationDenied)
            );
            drop(lease);
            assert_eq!(*calls.lock().unwrap(), ["subject", "action", "revoke"]);
        } else {
            let expected = match outcome {
                1 => NativeFailure::AuthorizationDenied,
                2 => NativeFailure::AuthorizationCancelled,
                _ => NativeFailure::AuthorizationUnavailable,
            };
            assert!(matches!(result, Err(failure) if failure == expected));
            assert_eq!(*calls.lock().unwrap(), ["subject", "action"]);
        }
    }
    let (scope, accounts, clock) = setup();
    let request = scope.original.clone();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let peer = PolkitPeer {
        outcome: 0,
        uid: 502,
        calls: calls.clone(),
    };
    let issuer = NativeAuthorizationIssuer::new(
        Arc::new(PolkitAuthorization::new(peer, scope)),
        accounts,
        clock,
    );
    let guard = native_management::helper_lifetime::HelperLifetimeGuard::install().unwrap();
    assert!(matches!(
        native_management::linux::authorize_native_request(&issuer, &request, guard.deadline()),
        Err(NativeFailure::IdentityMismatch)
    ));
    assert_eq!(*calls.lock().unwrap(), ["subject"]);
}

impl native_management::facade::operation_authority::NativeAssessmentScopes<Caller> for Arc<Peer> {
    type Scope = OriginalAssessment;
    fn retain(
        &self,
        caller: &Caller,
        request: &ManagementRequest,
        deadline: HelperDeadline,
    ) -> Result<Self::Scope, NativeFailure> {
        self.retain_assessment(caller, request, deadline)
    }
    fn request<'a>(&self, scope: &'a Self::Scope) -> &'a ManagementRequest {
        &scope.request
    }
    fn issued(&self, scope: &Self::Scope) -> Duration {
        scope.issued
    }
    fn recheck(
        &self,
        caller: &Caller,
        scope: &Self::Scope,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        self.recheck_caller(caller, scope, deadline)
    }
}

#[cfg(target_os = "macos")]
mod linked_local_right {
    use super::*;
    use std::sync::OnceLock;
    static SERIAL: Mutex<()> = Mutex::new(());
    static LIBRARY: OnceLock<usize> = OnceLock::new();
    fn native_fixture() {
        LIBRARY.get_or_init(|| {
            let scratch = tempfile::tempdir().unwrap();
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
            let library = scratch.path().join("management-fixture.dylib");
            let output = std::process::Command::new("/usr/bin/xcrun")
                .args([
                    "--sdk",
                    "macosx",
                    "clang",
                    "-std=c11",
                    "-Wall",
                    "-Wextra",
                    "-Werror",
                    "-DST_MANAGEMENT_TESTING",
                    "-dynamiclib",
                    "-mmacosx-version-min=26.0",
                ])
                .arg(root.join("native/macos/management.c"))
                .arg(root.join("tests/native_management_composition/local_right_fixture.c"))
                .args([
                    "-framework",
                    "Security",
                    "-framework",
                    "CoreFoundation",
                    "-o",
                ])
                .arg(&library)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .output()
                .unwrap();
            assert!(output.status.success(), "native fixture compilation failed");
            use std::os::unix::ffi::OsStrExt;
            let library = std::ffi::CString::new(library.as_os_str().as_bytes()).unwrap();
            // SAFETY: this private test dylib is compiled from the actual native boundary
            // plus injected OS peers. It contains no production admission or host mutation.
            let handle =
                unsafe { libc::dlopen(library.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
            assert!(!handle.is_null(), "native fixture linking failed");
            // Keep the linked object until test-process exit, through every native lease.
            handle as usize
        });
    }
    fn mode(value: u32) {
        native_fixture();
        // SAFETY: the private test symbol has this fixed signature and remains loaded.
        let symbol = unsafe {
            libc::dlsym(
                libc::RTLD_DEFAULT,
                c"st_management_composition_mode".as_ptr(),
            )
        };
        assert!(!symbol.is_null());
        let mode: unsafe extern "C" fn(u32) = unsafe { std::mem::transmute(symbol) };
        unsafe {
            mode(value);
        }
    }
    fn retired() -> u32 {
        // SAFETY: same private linked test object and exact fixed function ABI.
        let symbol = unsafe {
            libc::dlsym(
                libc::RTLD_DEFAULT,
                c"st_management_composition_retired".as_ptr(),
            )
        };
        assert!(!symbol.is_null());
        let read: unsafe extern "C" fn() -> u32 = unsafe { std::mem::transmute(symbol) };
        unsafe { read() }
    }
    impl native_management::macos::authorization::AuthorizationServicesCaller for AuditPeer {
        fn join_session(
            &self,
            caller: &Caller,
            deadline: HelperDeadline,
        ) -> Result<(), NativeFailure> {
            self.recheck(caller, deadline)
        }
    }
    fn execute(
        mode_value: u32,
        change_account_under_lock: bool,
    ) -> (Result<ManagementProgress, NativeFailure>, TestBackend, u32) {
        use native_management::facade::assembly::InstallationManagerAssembly;
        mode(mode_value);
        let mut owner = assembled_entry::owner(
            OperationTag::Start,
            native_management::cli_sudo::authorization::PrivilegedBroker::Sudo,
        );
        owner.change_account_under_lock = change_account_under_lock;
        let request = owner.scopes.original.clone();
        let journal = owner.journal.clone();
        let assembly = InstallationManagerAssembly::new(BuildIdentity::Development, owner);
        let guard = native_management::helper_lifetime::HelperLifetimeGuard::install().unwrap();
        let manager = assembly
            .assemble_gui(Arc::new(AuditPeer(501)), &request, guard.deadline())
            .unwrap();
        let mut outcome = None;
        manager.dispatch(
            guard,
            ManagerInvocation::GuiDemand,
            request,
            &mut |result, _| outcome = Some(result),
        );
        (outcome.unwrap(), journal, retired())
    }
    #[test]
    fn native_manager_linked_authorization_services_reaches_transaction_and_controller() {
        let _serial = SERIAL.lock().unwrap();
        let (result, owner, retired) = execute(0, false);
        assert_eq!(result.unwrap().state, ProgressState::Complete);
        assert_eq!(owner.0.lock().unwrap().started, [42]);
        assert_eq!(retired, 1);
    }
    #[test]
    fn native_manager_linked_denials_and_original_expiry_have_zero_effects() {
        let _serial = SERIAL.lock().unwrap();
        for (mode, failure) in [
            (1, NativeFailure::AuthorizationDenied),
            (2, NativeFailure::AuthorizationCancelled),
            (3, NativeFailure::AuthorizationUnavailable),
            (4, NativeFailure::AuthorizationDenied),
            (5, NativeFailure::StaleAssessment),
            (6, NativeFailure::AuthorizationDenied),
        ] {
            let (result, owner, retired) = execute(mode, false);
            assert_eq!(result, Err(failure));
            assert!(owner.0.lock().unwrap().events.is_empty());
            assert_eq!(retired, u32::from(mode != 6));
        }
    }
    #[test]
    fn native_manager_linked_account_incarnation_change_refuses_before_effects() {
        let _serial = SERIAL.lock().unwrap();
        let (result, owner, retired) = execute(0, true);
        assert_eq!(result, Err(NativeFailure::IdentityMismatch));
        assert!(owner.0.lock().unwrap().events.is_empty());
        assert_eq!(retired, 1);
    }
}

#[cfg(target_os = "linux")]
mod linked_polkit {
    use super::*;
    use native_management::linux::{
        polkit::PolkitOperationRights,
        system_polkit::{NativePolkitPolicy, NativePolkitProcessCaller, SystemPolkitPeer},
    };
    use std::sync::OnceLock;
    static SERIAL: Mutex<()> = Mutex::new(());
    static LIBRARY: OnceLock<usize> = OnceLock::new();
    impl NativePolkitProcessCaller for AuditPeer {
        fn process(&self, caller: &Caller) -> Result<(u32, u64), NativeFailure> {
            if caller.0 != self.0 {
                return Err(NativeFailure::IdentityMismatch);
            }
            Ok((777, 42))
        }
    }
    struct Policy;
    impl NativePolkitPolicy for Policy {
        fn recheck(&self, _: &ManagementRequest) -> Result<(), NativeFailure> {
            Ok(())
        }
    }
    fn fixture(mode: u32) {
        LIBRARY.get_or_init(|| {
            let scratch = tempfile::tempdir().unwrap();
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
            let library = scratch.path().join("polkit-fixture.so");
            let output = std::process::Command::new("cc")
                .args([
                    "-std=c11",
                    "-Wall",
                    "-Wextra",
                    "-Werror",
                    "-shared",
                    "-fPIC",
                    "-DST_MANAGEMENT_TESTING",
                    "-DST_MANAGEMENT_COMPOSITION_FIXTURE",
                ])
                .arg(root.join("native/linux/management/polkit.c"))
                .arg(root.join("native/macos/management.c"))
                .arg(root.join("../../tests/server_native/linux/management/polkit_tests.c"))
                .args(["-ldl", "-o"])
                .arg(&library)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "native polkit fixture compilation failed"
            );
            use std::os::unix::ffi::OsStrExt;
            let library = std::ffi::CString::new(library.as_os_str().as_bytes()).unwrap();
            // SAFETY: the private dylib contains the actual native boundary with a
            // test-only system-bus peer. It opens no native bus or privileged facility.
            let handle =
                unsafe { libc::dlopen(library.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
            assert!(!handle.is_null());
            handle as usize // Retain through every native token and test-process exit.
        });
        // SAFETY: the test-only setter has this exact ABI and remains loaded.
        let symbol = unsafe {
            libc::dlsym(
                libc::RTLD_DEFAULT,
                c"st_management_polkit_composition_mode".as_ptr(),
            )
        };
        assert!(!symbol.is_null());
        let set: unsafe extern "C" fn(u32) = unsafe { std::mem::transmute(symbol) };
        unsafe {
            set(mode);
        }
    }
    fn retired() -> u32 {
        // SAFETY: the loaded test-only counter has this exact ABI.
        let symbol = unsafe {
            libc::dlsym(
                libc::RTLD_DEFAULT,
                c"st_management_polkit_composition_retired".as_ptr(),
            )
        };
        assert!(!symbol.is_null());
        let read: unsafe extern "C" fn() -> u32 = unsafe { std::mem::transmute(symbol) };
        unsafe { read() }
    }
    fn execute(
        mode: u32,
        change_account_under_lock: bool,
    ) -> (Result<ManagementProgress, NativeFailure>, TestBackend, u32) {
        use native_management::facade::entry::NativeManagerDependencies;
        fixture(mode);
        let (mut peer, accounts, clock) = setup();
        let mut bytes = frame(OperationTag::Stop);
        bytes[56..88].copy_from_slice(&account(1).sha256());
        peer.original = ManagementRequest::decode(&bytes).unwrap();
        let request = peer.original.clone();
        let callers = Arc::new(AuditPeer(501));
        let native = SystemPolkitPeer::new(callers.clone(), Arc::new(Policy));
        let rights = PolkitOperationRights::new(native);
        let transactions = TransactionPeer {
            calls: Arc::new(Mutex::new(0)),
            accounts: accounts.clone(),
            change_account_under_lock,
        };
        let owner = TestBackend::new();
        let dependencies = NativeManagerDependencies::new(
            callers,
            Arc::new(peer),
            rights,
            accounts,
            clock,
            transactions,
            Owner(owner.clone()),
        );
        let guard = native_management::helper_lifetime::HelperLifetimeGuard::install().unwrap();
        let mut result = None;
        let manager: Box<dyn native_management::facade::entry::NativeManagerExecution> =
            Box::new(dependencies);
        manager.dispatch(
            guard,
            ManagerInvocation::Polkit(OperationTag::Stop),
            request,
            &mut |outcome, _| result = Some(outcome),
        );
        (result.unwrap(), owner, retired())
    }
    fn authorization_checks() -> u32 {
        // SAFETY: the loaded test-only counter has this exact ABI.
        let symbol = unsafe {
            libc::dlsym(
                libc::RTLD_DEFAULT,
                c"st_management_polkit_composition_checks".as_ptr(),
            )
        };
        assert!(!symbol.is_null());
        let read: unsafe extern "C" fn() -> u32 = unsafe { std::mem::transmute(symbol) };
        unsafe { read() }
    }
    #[test]
    fn native_manager_pkexec_consumes_authorization_without_second_conversation() {
        let _serial = SERIAL.lock().unwrap();
        // pkexec has already completed its one-shot auth_admin conversation.
        // The manager must consume that retained execution, not ask polkit again.
        fixture(0);
        let owner = assembled_entry::owner(
            OperationTag::Stop,
            native_management::cli_sudo::authorization::PrivilegedBroker::Pkexec(
                OperationTag::Stop,
            ),
        );
        let journal = owner.journal.clone();
        let execution = owner.execution.clone();
        let result = assembled_entry::execute(owner, ManagerInvocation::Polkit(OperationTag::Stop));
        assert_eq!(result.unwrap().state, ProgressState::Complete);
        assert_eq!(*execution.retained.lock().unwrap(), 1);
        assert!(journal.0.lock().unwrap().events.contains(&"bootout"));
        assert_eq!(authorization_checks(), 0);
    }
    #[test]
    fn native_manager_linked_polkit_reaches_transaction_and_controller() {
        let _serial = SERIAL.lock().unwrap();
        for mode in [0, 8] {
            let (result, owner, retired) = execute(mode, false);
            assert_eq!(result.unwrap().state, ProgressState::Complete);
            assert_eq!(owner.0.lock().unwrap().intent, DesiredServerState::Stopped);
            assert!(owner.0.lock().unwrap().events.contains(&"bootout"));
            assert_eq!(retired, 1);
        }
    }
    #[test]
    fn native_manager_linked_polkit_denials_and_bad_native_evidence_have_zero_effects() {
        let _serial = SERIAL.lock().unwrap();
        for (mode, failure) in [
            (1, NativeFailure::AuthorizationDenied),
            (2, NativeFailure::AuthorizationCancelled),
            (3, NativeFailure::AuthorizationUnavailable),
            (4, NativeFailure::AuthorizationUnavailable),
            (5, NativeFailure::AuthorizationUnavailable),
            (6, NativeFailure::AuthorizationUnavailable),
            (7, NativeFailure::AuthorizationUnavailable),
            (9, NativeFailure::AuthorizationDenied),
            (10, NativeFailure::AuthorizationCancelled),
        ] {
            let (result, owner, retired) = execute(mode, false);
            assert_eq!(result, Err(failure));
            assert!(owner.0.lock().unwrap().events.is_empty());
            assert_eq!(retired, 1);
        }
    }
    #[test]
    fn native_manager_linked_polkit_account_incarnation_change_refuses_before_effects() {
        let _serial = SERIAL.lock().unwrap();
        let (result, owner, retired) = execute(0, true);
        assert_eq!(result, Err(NativeFailure::IdentityMismatch));
        assert!(owner.0.lock().unwrap().events.is_empty());
        assert_eq!(retired, 1);
    }
}

#[test]
fn manager_injected_assembly_cannot_bypass_the_production_source_gate() {
    use native_management::facade::entry::{
        NativeManagerAssembly, NativeManagerExecution, production_with_assembly,
    };
    struct Assembly(Mutex<u32>);
    impl NativeManagerAssembly for Assembly {
        fn broker(
            &self,
            _: Arc<native_management::facade::socket_caller::BrokerCallerPeer>,
            _: ManagerInvocation,
            _: &ManagementRequest,
            _: HelperDeadline,
        ) -> Result<Box<dyn NativeManagerExecution>, NativeFailure> {
            *self.0.lock().unwrap() += 1;
            Err(NativeFailure::AuthorizationUnavailable)
        }
        #[cfg(target_os = "macos")]
        fn gui(
            &self,
            _: Arc<native_management::macos::demand::MachDemandCallerPeer>,
            _: &ManagementRequest,
            _: HelperDeadline,
        ) -> Result<Box<dyn NativeManagerExecution>, NativeFailure> {
            *self.0.lock().unwrap() += 1;
            Err(NativeFailure::AuthorizationUnavailable)
        }
    }
    let assembly = Assembly(Mutex::new(0));
    let guard = native_management::helper_lifetime::HelperLifetimeGuard::install().unwrap();
    let code = production_with_assembly(
        &["--gui-demand".into()],
        guard,
        BuildIdentity::Development,
        &assembly,
    );
    assert_eq!(code, NativeFailure::CustodyUnavailable as u8);
    assert_eq!(*assembly.0.lock().unwrap(), 0);
}

mod assembled_entry {
    use super::*;
    use native_management::{
        cli_sudo::authorization::{PrivilegedBroker, PrivilegedManagerCustody},
        facade::assembly::{
            InstallationManagerAssembly, NativeBrokerManagerOwner, NativeManagerOwner,
            NativeManagerPorts,
        },
    };

    pub(super) struct ReceivedCaller(pub u32);
    impl NativeCallerAuthentication for ReceivedCaller {
        type Caller = Caller;
        fn bind(
            &self,
            _: AuthorizationRoute,
            deadline: HelperDeadline,
        ) -> Result<Caller, NativeFailure> {
            deadline.check()?;
            Ok(Caller(self.0))
        }
        fn uid(&self, caller: &Caller) -> Result<u32, NativeFailure> {
            Ok(caller.0)
        }
        fn recheck(&self, caller: &Caller, deadline: HelperDeadline) -> Result<(), NativeFailure> {
            deadline.check()?;
            if caller.0 != self.0 {
                return Err(NativeFailure::ProcessIdentityMismatch);
            }
            Ok(())
        }
    }

    pub(super) struct Execution {
        request: ManagementRequest,
        broker: PrivilegedBroker,
        failure: Option<NativeFailure>,
        pub retained: Mutex<u32>,
        invalidate_confirmation: bool,
        journal: TestBackend,
    }
    pub(super) struct ExecutionLease {
        request: ManagementRequest,
        broker: PrivilegedBroker,
    }
    impl PrivilegedManagerCustody<Caller> for Arc<Execution> {
        type Lease = ExecutionLease;
        fn retain_current(
            &self,
            caller: &Caller,
            request: &ManagementRequest,
            broker: PrivilegedBroker,
            deadline: HelperDeadline,
        ) -> Result<Self::Lease, NativeFailure> {
            deadline.check()?;
            *self.retained.lock().unwrap() += 1;
            if let Some(failure) = self.failure {
                return Err(failure);
            }
            if caller.0 != 501 || request != &self.request || broker != self.broker {
                return Err(NativeFailure::IdentityMismatch);
            }
            if self.invalidate_confirmation {
                self.journal.0.lock().unwrap().revalidation =
                    Some(NativeFailure::GenerationMismatch);
            }
            Ok(ExecutionLease {
                request: self.request.clone(),
                broker,
            })
        }
        fn recheck(
            &self,
            lease: &Self::Lease,
            caller: &Caller,
            request: &ManagementRequest,
            broker: PrivilegedBroker,
            deadline: HelperDeadline,
        ) -> Result<(), NativeFailure> {
            deadline.check()?;
            if caller.0 != 501
                || request != &lease.request
                || broker != lease.broker
                || broker != self.broker
            {
                return Err(NativeFailure::IdentityMismatch);
            }
            Ok(())
        }
    }
    pub(super) struct InstallationOwner {
        pub scopes: Arc<Peer>,
        pub accounts: Arc<Accounts>,
        pub clock: Arc<Clock>,
        pub journal: TestBackend,
        pub execution: Arc<Execution>,
        pub transactions: Arc<Mutex<u32>>,
        pub change_account_under_lock: bool,
        pub retained: Arc<Mutex<u32>>,
    }
    impl<C: NativeCallerAuthentication<Caller = Caller>> NativeManagerOwner<C> for InstallationOwner {
        type Scopes = Arc<Peer>;
        type Transactions = TransactionPeer;
        type Journal = Owner;
        fn retain(
            &self,
            _: &C,
            _: &ManagementRequest,
            deadline: HelperDeadline,
        ) -> Result<
            NativeManagerPorts<Self::Scopes, Self::Transactions, Self::Journal>,
            NativeFailure,
        > {
            deadline.check()?;
            *self.retained.lock().unwrap() += 1;
            Ok(NativeManagerPorts::new(
                self.scopes.clone(),
                self.accounts.clone(),
                self.clock.clone(),
                TransactionPeer {
                    calls: self.transactions.clone(),
                    accounts: self.accounts.clone(),
                    change_account_under_lock: self.change_account_under_lock,
                },
                Owner(self.journal.clone()),
            ))
        }
    }
    impl<C: NativeCallerAuthentication<Caller = Caller>> NativeBrokerManagerOwner<C>
        for InstallationOwner
    {
        type Custody = Arc<Execution>;
        fn custody(&self) -> Result<Self::Custody, NativeFailure> {
            Ok(self.execution.clone())
        }
    }
    pub(super) fn owner(operation: OperationTag, broker: PrivilegedBroker) -> InstallationOwner {
        let (mut peer, accounts, clock) = setup();
        let mut bytes = frame(operation);
        bytes[56..88].copy_from_slice(&account(1).sha256());
        peer.original = ManagementRequest::decode(&bytes).unwrap();
        let journal = TestBackend::new();
        let execution = Arc::new(Execution {
            request: peer.original.clone(),
            broker,
            failure: None,
            retained: Mutex::new(0),
            invalidate_confirmation: false,
            journal: journal.clone(),
        });
        InstallationOwner {
            scopes: Arc::new(peer),
            accounts,
            clock,
            journal,
            execution,
            transactions: Arc::new(Mutex::new(0)),
            change_account_under_lock: false,
            retained: Arc::new(Mutex::new(0)),
        }
    }
    pub(super) fn execute(
        owner: InstallationOwner,
        invocation: ManagerInvocation,
    ) -> Result<ManagementProgress, NativeFailure> {
        let request = owner.scopes.original.clone();
        let assembly = InstallationManagerAssembly::new(BuildIdentity::Development, owner);
        let guard = native_management::helper_lifetime::HelperLifetimeGuard::install().unwrap();
        let manager = assembly.assemble_broker(
            Arc::new(ReceivedCaller(501)),
            invocation,
            &request,
            guard.deadline(),
        )?;
        let mut result = None;
        manager.dispatch(guard, invocation, request, &mut |outcome, _| {
            result = Some(outcome)
        });
        result.unwrap()
    }
    #[test]
    fn native_manager_assembled_entry_reaches_transaction_authority_and_controller() {
        for operation in [
            OperationTag::Start,
            OperationTag::Stop,
            OperationTag::Install,
        ] {
            let owner = owner(operation, PrivilegedBroker::Sudo);
            let journal = owner.journal.clone();
            let transactions = owner.transactions.clone();
            let retained = owner.retained.clone();
            assert_eq!(
                execute(owner, ManagerInvocation::Sudo).unwrap().state,
                ProgressState::Complete
            );
            assert_eq!(*retained.lock().unwrap(), 1);
            assert_eq!(*transactions.lock().unwrap(), 1);
            assert!(
                journal
                    .0
                    .lock()
                    .unwrap()
                    .events
                    .contains(&"journal-admitted")
            );
        }
    }
    fn routes(operation: OperationTag) -> Vec<(PrivilegedBroker, ManagerInvocation)> {
        #[cfg(target_os = "linux")]
        {
            vec![
                (PrivilegedBroker::Sudo, ManagerInvocation::Sudo),
                (
                    PrivilegedBroker::Pkexec(operation),
                    ManagerInvocation::Polkit(operation),
                ),
            ]
        }
        #[cfg(target_os = "macos")]
        {
            let _ = operation;
            vec![(PrivilegedBroker::Sudo, ManagerInvocation::Sudo)]
        }
    }
    #[test]
    fn native_manager_assembled_entry_account_incarnation_change_refuses_before_effects() {
        for (broker, invocation) in routes(OperationTag::Stop) {
            let mut owner = owner(OperationTag::Stop, broker);
            owner.change_account_under_lock = true;
            let journal = owner.journal.clone();
            let transactions = owner.transactions.clone();
            assert_eq!(
                execute(owner, invocation),
                Err(NativeFailure::IdentityMismatch)
            );
            assert_eq!(*transactions.lock().unwrap(), 1);
            assert!(journal.0.lock().unwrap().events.is_empty());
        }
    }
    #[test]
    fn native_manager_assembled_entry_revalidates_destructive_confirmation_after_native_authority()
    {
        for (broker, invocation) in routes(OperationTag::Stop) {
            let mut owner = owner(OperationTag::Stop, broker);
            Arc::get_mut(&mut owner.execution)
                .unwrap()
                .invalidate_confirmation = true;
            let journal = owner.journal.clone();
            let transactions = owner.transactions.clone();
            let native = owner.execution.clone();
            assert_eq!(
                execute(owner, invocation),
                Err(NativeFailure::GenerationMismatch)
            );
            assert_eq!(*native.retained.lock().unwrap(), 1);
            assert_eq!(*transactions.lock().unwrap(), 0);
            assert!(journal.0.lock().unwrap().events.is_empty());
        }
    }
    #[test]
    fn native_manager_assembled_entry_native_denial_cancellation_and_failed_custody_have_zero_effects()
     {
        for (broker, invocation) in routes(OperationTag::Stop) {
            for failure in [
                NativeFailure::AuthorizationDenied,
                NativeFailure::AuthorizationCancelled,
                NativeFailure::AuthorizationUnavailable,
                NativeFailure::ExecutionBindingUnproven,
            ] {
                let mut owner = owner(OperationTag::Stop, broker);
                Arc::get_mut(&mut owner.execution).unwrap().failure = Some(failure);
                let journal = owner.journal.clone();
                let transactions = owner.transactions.clone();
                assert_eq!(execute(owner, invocation), Err(failure));
                assert_eq!(*transactions.lock().unwrap(), 0);
                assert!(journal.0.lock().unwrap().events.is_empty());
            }
        }
    }
    #[test]
    fn native_manager_assembled_entry_original_expiry_and_execution_substitution_have_zero_effects()
    {
        for (broker, invocation) in routes(OperationTag::Stop) {
            for expired in [true, false] {
                let mut owner = owner(OperationTag::Stop, broker);
                if expired {
                    // Native authorization completed after the original scope expired.
                    *owner.clock.0.lock().unwrap() = Duration::from_secs(31);
                } else {
                    let execution = Arc::get_mut(&mut owner.execution).unwrap();
                    let mut bytes = execution.request.encode();
                    bytes[88] ^= 1;
                    execution.request = ManagementRequest::decode(&bytes).unwrap();
                }
                let journal = owner.journal.clone();
                let transactions = owner.transactions.clone();
                assert_eq!(
                    execute(owner, invocation),
                    Err(if expired {
                        NativeFailure::StaleAssessment
                    } else {
                        NativeFailure::IdentityMismatch
                    })
                );
                assert_eq!(*transactions.lock().unwrap(), 0);
                assert!(journal.0.lock().unwrap().events.is_empty());
            }
        }
    }
    #[test]
    fn native_manager_assembled_entry_rejects_wrong_build_or_fixed_operation_before_owner_access() {
        for wrong_build in [true, false] {
            let owner = owner(OperationTag::Stop, PrivilegedBroker::Sudo);
            let request = owner.scopes.original.clone();
            let retained = owner.retained.clone();
            let journal = owner.journal.clone();
            let build = if wrong_build {
                BuildIdentity::Release
            } else {
                BuildIdentity::Development
            };
            let assembly = InstallationManagerAssembly::new(build, owner);
            let guard = native_management::helper_lifetime::HelperLifetimeGuard::install().unwrap();
            let invocation = if wrong_build {
                ManagerInvocation::Sudo
            } else {
                ManagerInvocation::Polkit(OperationTag::Start)
            };
            assert!(matches!(
                assembly.assemble_broker(
                    Arc::new(ReceivedCaller(501)),
                    invocation,
                    &request,
                    guard.deadline()
                ),
                Err(NativeFailure::IdentityMismatch)
            ));
            assert_eq!(*retained.lock().unwrap(), 0);
            assert!(journal.0.lock().unwrap().events.is_empty());
        }
    }
}
