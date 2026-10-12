//! Actual controller backend over native authorization and the installation owner's journal port.

use super::authorization::{
    NativeAuthorizationBoundary, NativeAuthorizationIssuer, NativeAuthorizationLease,
    NativeCallerLease,
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

/// The installation owner's caller-authenticated existing-request lookup.
/// This cannot admit a request. The owner compares every immutable request field
/// under the same protected transaction authority used by fresh admission.
pub trait NativeRetainedRequestLookup:
    ManagementBackend<Authorization = NativeAuthorizationLease>
{
    /// Returns only an exactly bound, durably admitted original operation.
    fn lookup_authenticated(
        &self,
        request: &ManagementRequest,
        caller: &NativeCallerLease,
        deadline: HelperDeadline,
    ) -> Result<Option<JournalAdmission<Self::Transaction>>, NativeFailure>;
}

/// Composes OS authorization with the existing installation/lifecycle owner.
/// The owner must consume the live lease under its NativeTransactionAuthority lock at admission.
/// No request record or lifecycle journal is stored by this adapter.
pub struct NativeManagerBackend<P, J> {
    issuer: NativeAuthorizationIssuer<P>,
    owner: J,
}
impl<P: NativeAuthorizationBoundary, J: NativeRetainedRequestLookup> NativeManagerBackend<P, J> {
    /// Injects native authentication and the installation owner's real transaction implementation.
    pub fn new(issuer: NativeAuthorizationIssuer<P>, owner: J) -> Self {
        Self { issuer, owner }
    }
}
impl<P: NativeAuthorizationBoundary, J: NativeRetainedRequestLookup> ManagementBackend
    for NativeManagerBackend<P, J>
{
    type Authorization = NativeAuthorizationLease;
    type Transaction = J::Transaction;
    fn lookup_retained(
        &self,
        request: &ManagementRequest,
        route: AuthorizationRoute,
        deadline: HelperDeadline,
    ) -> Result<Option<JournalAdmission<Self::Transaction>>, NativeFailure> {
        let caller = self.issuer.authenticate(request, route, deadline)?;
        caller.recheck(deadline)?;
        let existing = self
            .owner
            .lookup_authenticated(request, &caller, deadline)?;
        caller.recheck(deadline)?;
        if let Some(JournalAdmission::Transaction(transaction)) = &existing
            && self.owner.request(transaction) != request
        {
            return Err(NativeFailure::IdentityMismatch);
        }
        Ok(existing)
    }
    fn authorize(
        &self,
        request: &ManagementRequest,
        route: AuthorizationRoute,
        deadline: HelperDeadline,
    ) -> Result<Self::Authorization, NativeFailure> {
        self.issuer.authorize(request, route, deadline)
    }
    fn lookup_existing(
        &self,
        request: &ManagementRequest,
        authority: &Self::Authorization,
        deadline: HelperDeadline,
    ) -> Result<Option<JournalAdmission<Self::Transaction>>, NativeFailure> {
        bind(authority, request, deadline)?;
        self.owner.lookup_existing(request, authority, deadline)
    }
    fn revalidate(
        &self,
        request: &ManagementRequest,
        authority: &Self::Authorization,
        deadline: HelperDeadline,
    ) -> Result<Duration, NativeFailure> {
        bind(authority, request, deadline)?;
        let age = self.owner.revalidate(request, authority, deadline)?;
        bind(authority, request, deadline)?;
        Ok(age)
    }
    fn admit(
        &self,
        request: &ManagementRequest,
        authority: Self::Authorization,
        deadline: HelperDeadline,
    ) -> Result<JournalAdmission<Self::Transaction>, NativeFailure> {
        bind(&authority, request, deadline)?;
        self.owner.admit(request, authority, deadline)
    }
    fn resolve(
        &self,
        request: [u8; 16],
        deadline: HelperDeadline,
    ) -> Result<JournalAdmission<Self::Transaction>, NativeFailure> {
        deadline.check()?;
        // The owner authenticates the resolving principal. Resolution cannot admit a fresh request.
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
    fn commit_restart(
        &self,
        transaction: &mut Self::Transaction,
        deadline: HelperDeadline,
    ) -> IntentDisposition {
        self.owner.commit_restart(transaction, deadline)
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
fn bind(
    authority: &NativeAuthorizationLease,
    request: &ManagementRequest,
    deadline: HelperDeadline,
) -> Result<(), NativeFailure> {
    if authority.request() != request {
        return Err(NativeFailure::IdentityMismatch);
    }
    authority.recheck(deadline)
}
