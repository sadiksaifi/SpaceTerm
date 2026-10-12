//! Management admission over the installation owner's seeded transaction authority.

use super::authorization::{NativeAuthorizationLease, NativeCallerLease};
use super::backend::NativeRetainedRequestLookup;
use crate::native_deployment::ports::{
    AuthorizedNativeRequest, NativeTransactionAuthority, NativeTransactionLease,
};
use crate::native_management::{
    controller::{
        AuthorizationRoute, IntentDisposition, JournalAdmission, ManagementBackend,
        ManagementPhase, ManagementProgress, Quiescence,
    },
    helper_lifetime::HelperDeadline,
    request::ManagementRequest,
};
use crate::{native_deployment::contracts::NativeFailure, startup::DesiredServerState};
use std::time::Duration;

/// Coordinator's private conversion from a live native lease to common authority.
/// It must own the lease. Decoded requests, booleans, and comparison facts cannot implement it.
pub trait NativeAuthorizationBridge: Send + Sync {
    /// Consumes a natively issued lease into the common nonserializable representation.
    fn bind(
        &self,
        lease: NativeAuthorizationLease,
    ) -> Result<AuthorizedNativeRequest, NativeFailure>;
    /// Rechecks the owned original lease after the transaction lock is acquired.
    fn recheck(&self, request: &AuthorizedNativeRequest) -> Result<(), NativeFailure>;
    /// Returns the independently reconstructed original request retained in the common wrapper.
    fn request<'a>(&self, authority: &'a AuthorizedNativeRequest) -> &'a ManagementRequest;
}

/// Retained common request and the installation owner's single native transaction lease.
/// No constructor accepts caller-supplied record, descriptor, or authorization facts.
pub struct AuthorizedManagementTransaction {
    authorization: AuthorizedNativeRequest,
    transaction: NativeTransactionLease,
}
impl AuthorizedManagementTransaction {
    /// The common request owns its actual OS authorization lease through admission.
    pub fn authorization(&self) -> &AuthorizedNativeRequest {
        &self.authorization
    }
    /// Retained installation-owner authority, not a second management lock or journal.
    pub fn transaction(&self) -> &NativeTransactionLease {
        &self.transaction
    }
}
impl std::fmt::Debug for AuthorizedManagementTransaction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AuthorizedManagementTransaction(..)")
    }
}

/// Narrow native serialization boundary. The system implementation uses the seeded A port.
/// Injected peers exercise admission without manufacturing an uninhabited seed authority.
pub trait ManagerTransactionPeer: Send + Sync {
    /// Noncloneable authority held through durable journal admission.
    type Lease;
    /// Acquires the installation owner's native transaction with the actual native grant.
    fn begin(
        &self,
        lease: NativeAuthorizationLease,
        deadline: HelperDeadline,
    ) -> Result<Self::Lease, NativeFailure>;
    /// Rechecks retained serialization and original native authorization before admission.
    fn recheck(&self, lease: &Self::Lease, deadline: HelperDeadline) -> Result<(), NativeFailure>;
    /// Exact original request, retained by the authority rather than reconstructed from a DTO.
    fn request<'a>(&self, lease: &'a Self::Lease) -> &'a ManagementRequest;
}

/// Actual adapter to the seeded account/installation transaction representation.
pub struct NativeTransactionPeer<A, B> {
    authority: A,
    bridge: B,
}
impl<A: NativeTransactionAuthority, B: NativeAuthorizationBridge> NativeTransactionPeer<A, B> {
    /// Injects the existing installation owner and coordinator's private representation bridge.
    pub fn new(authority: A, bridge: B) -> Self {
        Self { authority, bridge }
    }
}
impl<A: NativeTransactionAuthority, B: NativeAuthorizationBridge> ManagerTransactionPeer
    for NativeTransactionPeer<A, B>
{
    type Lease = AuthorizedManagementTransaction;
    fn begin(
        &self,
        lease: NativeAuthorizationLease,
        deadline: HelperDeadline,
    ) -> Result<Self::Lease, NativeFailure> {
        lease.recheck(deadline)?;
        let authorization = self.bridge.bind(lease)?;
        self.bridge.recheck(&authorization)?;
        deadline.check()?;
        let transaction = self.authority.begin(&authorization)?;
        let retained = AuthorizedManagementTransaction {
            authorization,
            transaction,
        };
        self.recheck(&retained, deadline)?;
        Ok(retained)
    }
    fn recheck(&self, lease: &Self::Lease, deadline: HelperDeadline) -> Result<(), NativeFailure> {
        deadline.check()?;
        self.authority.revalidate(&lease.transaction)?;
        self.bridge.recheck(&lease.authorization)?;
        deadline.check()
    }
    fn request<'a>(&self, lease: &'a Self::Lease) -> &'a ManagementRequest {
        self.bridge.request(&lease.authorization)
    }
}

/// The existing installation journal's management operations. This adapter stores no journal.
pub trait ManagerJournal<L>: ManagementBackend<Authorization = L> {
    /// Caller-authenticated exact lookup, under the installation owner's retained authority.
    fn lookup_authenticated(
        &self,
        request: &ManagementRequest,
        caller: &NativeCallerLease,
        deadline: HelperDeadline,
    ) -> Result<Option<JournalAdmission<Self::Transaction>>, NativeFailure>;
    /// Fresh-grant exact lookup in the same native owner. It must never admit an operation.
    fn lookup_authorized(
        &self,
        request: &ManagementRequest,
        authorization: &NativeAuthorizationLease,
        deadline: HelperDeadline,
    ) -> Result<Option<JournalAdmission<Self::Transaction>>, NativeFailure>;
    /// Checks original confirmation and current generations. Admission rechecks under the native lock.
    fn revalidate_original(
        &self,
        request: &ManagementRequest,
        authorization: &NativeAuthorizationLease,
        deadline: HelperDeadline,
    ) -> Result<Duration, NativeFailure>;
}

/// Controller backend adapter that acquires A's transaction before calling the journal owner.
pub struct TransactionBackedManager<P, J> {
    peer: P,
    owner: J,
}
impl<P: ManagerTransactionPeer, J: ManagerJournal<P::Lease>> TransactionBackedManager<P, J> {
    /// Injects native serialization and the installation owner's existing lifecycle/journal operations.
    pub fn new(peer: P, owner: J) -> Self {
        Self { peer, owner }
    }
}
impl<P: ManagerTransactionPeer, J: ManagerJournal<P::Lease>> NativeRetainedRequestLookup
    for TransactionBackedManager<P, J>
{
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
        self.owner.lookup_authenticated(request, caller, deadline)
    }
}
impl<P: ManagerTransactionPeer, J: ManagerJournal<P::Lease>> ManagementBackend
    for TransactionBackedManager<P, J>
{
    type Authorization = NativeAuthorizationLease;
    type Transaction = J::Transaction;
    fn authorize(
        &self,
        _request: &ManagementRequest,
        _route: AuthorizationRoute,
        _deadline: HelperDeadline,
    ) -> Result<Self::Authorization, NativeFailure> {
        // Only NativeManagerBackend's issuer authenticates callers. This journal adapter cannot issue authority.
        Err(NativeFailure::AuthorizationUnavailable)
    }
    fn lookup_existing(
        &self,
        request: &ManagementRequest,
        authority: &Self::Authorization,
        deadline: HelperDeadline,
    ) -> Result<Option<JournalAdmission<Self::Transaction>>, NativeFailure> {
        check_request(authority, request, deadline)?;
        self.owner.lookup_authorized(request, authority, deadline)
    }
    fn revalidate(
        &self,
        request: &ManagementRequest,
        authority: &Self::Authorization,
        deadline: HelperDeadline,
    ) -> Result<Duration, NativeFailure> {
        check_request(authority, request, deadline)?;
        self.owner.revalidate_original(request, authority, deadline)
    }
    fn admit(
        &self,
        request: &ManagementRequest,
        authority: Self::Authorization,
        deadline: HelperDeadline,
    ) -> Result<JournalAdmission<Self::Transaction>, NativeFailure> {
        check_request(&authority, request, deadline)?;
        let lease = self.peer.begin(authority, deadline)?;
        self.peer.recheck(&lease, deadline)?;
        if self.peer.request(&lease) != request {
            return Err(NativeFailure::IdentityMismatch);
        }
        // The journal owner consumes the exact common lease and persists prerequisites before effects.
        self.owner.admit(request, lease, deadline)
    }
    fn resolve(
        &self,
        request: [u8; 16],
        deadline: HelperDeadline,
    ) -> Result<JournalAdmission<Self::Transaction>, NativeFailure> {
        deadline.check()?;
        self.owner.resolve(request, deadline)
    }
    fn request<'a>(&self, transaction: &'a Self::Transaction) -> &'a ManagementRequest {
        self.owner.request(transaction)
    }
    fn phase(&self, transaction: &Self::Transaction) -> ManagementPhase {
        self.owner.phase(transaction)
    }
    fn desired_state(
        &self,
        transaction: &Self::Transaction,
        deadline: HelperDeadline,
    ) -> Result<DesiredServerState, NativeFailure> {
        self.owner.desired_state(transaction, deadline)
    }
    fn commit_stopped(
        &self,
        transaction: &mut Self::Transaction,
        deadline: HelperDeadline,
    ) -> IntentDisposition {
        self.owner.commit_stopped(transaction, deadline)
    }
    fn suspend_automatic_start(
        &self,
        transaction: &mut Self::Transaction,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        self.owner.suspend_automatic_start(transaction, deadline)
    }
    fn begin_quiescence(
        &self,
        transaction: &mut Self::Transaction,
        deadline: HelperDeadline,
    ) -> Result<Quiescence, NativeFailure> {
        self.owner.begin_quiescence(transaction, deadline)
    }
    fn poll_quiescence(
        &self,
        transaction: &mut Self::Transaction,
        deadline: HelperDeadline,
    ) -> Result<Quiescence, NativeFailure> {
        self.owner.poll_quiescence(transaction, deadline)
    }
    fn retire_supervision(
        &self,
        transaction: &mut Self::Transaction,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        self.owner.retire_supervision(transaction, deadline)
    }
    fn finish_stop(
        &self,
        transaction: &mut Self::Transaction,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        self.owner.finish_stop(transaction, deadline)
    }
    fn start_approved(
        &self,
        transaction: &mut Self::Transaction,
        deadline: HelperDeadline,
    ) -> Result<ManagementProgress, NativeFailure> {
        self.owner.start_approved(transaction, deadline)
    }
    fn restart_approved(
        &self,
        transaction: &mut Self::Transaction,
        deadline: HelperDeadline,
    ) -> Result<ManagementProgress, NativeFailure> {
        self.owner.restart_approved(transaction, deadline)
    }
    fn apply_owned_operation(
        &self,
        transaction: &mut Self::Transaction,
        deadline: HelperDeadline,
    ) -> Result<ManagementProgress, NativeFailure> {
        self.owner.apply_owned_operation(transaction, deadline)
    }
}
fn check_request(
    lease: &NativeAuthorizationLease,
    request: &ManagementRequest,
    deadline: HelperDeadline,
) -> Result<(), NativeFailure> {
    if lease.request() != request {
        return Err(NativeFailure::IdentityMismatch);
    }
    lease.recheck(deadline)
}
