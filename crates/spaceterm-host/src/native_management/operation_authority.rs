//! Native rights composed with the installation owner's original assessment.

use super::authorization::{NativeAuthorizationBoundary, NativeGrantPolicy};
use super::caller::NativeCallerAuthentication;
use crate::native_deployment::contracts::NativeFailure;
use crate::native_management::{
    controller::AuthorizationRoute,
    helper_lifetime::HelperDeadline,
    request::{ManagementRequest, OperationTag},
};
use crate::startup::BuildIdentity;
use std::time::Duration;

/// Original assessment access supplied by the installation owner, not a second journal.
/// Implementations retain the original caller, boot, request, confirmation and issuance time.
pub trait NativeAssessmentScopes<C>: Send + Sync + 'static {
    /// Nonserializable retained original scope.
    type Scope: Send + Sync + 'static;
    /// Retains only the exact original request authenticated for this caller.
    fn retain(
        &self,
        caller: &C,
        request: &ManagementRequest,
        deadline: HelperDeadline,
    ) -> Result<Self::Scope, NativeFailure>;
    /// Independently reconstructed immutable request.
    fn request<'a>(&self, scope: &'a Self::Scope) -> &'a ManagementRequest;
    /// Original boot-bound issuance time, never authentication completion.
    fn issued(&self, scope: &Self::Scope) -> Duration;
    /// Rechecks the original caller and assessment without admitting a mutation.
    fn recheck(
        &self,
        caller: &C,
        scope: &Self::Scope,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure>;
}

/// Operation-specific native authorization. The native handle stays in this helper.
/// A root UID, request, successful broker exit or external form cannot implement this port.
pub trait NativeOperationRights<C>: Send + Sync + 'static {
    /// Owned native right or retained authenticated broker execution proof.
    type Right: Send + Sync + 'static;
    /// Requests/verifies the locally derived exact right for the actual caller and original scope.
    fn acquire(
        &self,
        caller: &C,
        request: &ManagementRequest,
        issued: Duration,
        route: AuthorizationRoute,
        deadline: HelperDeadline,
    ) -> Result<Self::Right, NativeFailure>;
    /// Effective supported native rule. Password-authenticated caches remain allowed.
    fn policy(&self, right: &Self::Right) -> Result<NativeGrantPolicy, NativeFailure>;
    /// Rechecks the same native handle, subject and exact request without another prompt.
    fn recheck(
        &self,
        caller: &C,
        right: &Self::Right,
        request: &ManagementRequest,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure>;
    /// Destroys the owned right on denial, expiry, cancellation or scope retirement.
    fn revoke(&self, right: &mut Self::Right);
}

/// Fixed native caller, original assessment and operation-right composition.
/// Test peers are selected only in test composition; production selects native adapters.
pub struct ScopedManagementAuthorization<C, S, R> {
    callers: C,
    scopes: S,
    rights: R,
}
impl<C, S, R> ScopedManagementAuthorization<C, S, R> {
    /// Injects the OS caller adapter and the installation owner's original scope.
    pub fn new(callers: C, scopes: S, rights: R) -> Self {
        Self {
            callers,
            scopes,
            rights,
        }
    }
}

/// Native handle plus its exact original request. It has no public constructor or exported form.
pub struct RequestBoundRight<R> {
    native: R,
    request: ManagementRequest,
}
impl<C, S, R> NativeAuthorizationBoundary for ScopedManagementAuthorization<C, S, R>
where
    C: NativeCallerAuthentication,
    S: NativeAssessmentScopes<C::Caller>,
    R: NativeOperationRights<C::Caller>,
{
    type Caller = C::Caller;
    type Assessment = S::Scope;
    type Grant = RequestBoundRight<R::Right>;
    fn bind_caller(
        &self,
        route: AuthorizationRoute,
        deadline: HelperDeadline,
    ) -> Result<Self::Caller, NativeFailure> {
        self.callers.bind(route, deadline)
    }
    fn retain_assessment(
        &self,
        caller: &Self::Caller,
        request: &ManagementRequest,
        deadline: HelperDeadline,
    ) -> Result<Self::Assessment, NativeFailure> {
        self.callers.recheck(caller, deadline)?;
        if self.callers.uid(caller)? != request.key().account_uid {
            return Err(NativeFailure::IdentityMismatch);
        }
        let scope = self.scopes.retain(caller, request, deadline)?;
        if self.scopes.request(&scope) != request {
            return Err(NativeFailure::IdentityMismatch);
        }
        self.recheck_caller(caller, &scope, deadline)?;
        Ok(scope)
    }
    fn request<'a>(&self, assessment: &'a Self::Assessment) -> &'a ManagementRequest {
        self.scopes.request(assessment)
    }
    fn issued(&self, assessment: &Self::Assessment) -> Duration {
        self.scopes.issued(assessment)
    }
    fn recheck_caller(
        &self,
        caller: &Self::Caller,
        assessment: &Self::Assessment,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        self.callers.recheck(caller, deadline)?;
        if self.callers.uid(caller)? != self.request(assessment).key().account_uid {
            return Err(NativeFailure::IdentityMismatch);
        }
        self.scopes.recheck(caller, assessment, deadline)
    }
    fn verify_right(
        &self,
        caller: &Self::Caller,
        assessment: &Self::Assessment,
        route: AuthorizationRoute,
        deadline: HelperDeadline,
    ) -> Result<Self::Grant, NativeFailure> {
        self.recheck_caller(caller, assessment, deadline)?;
        let request = self.request(assessment);
        let mut native =
            self.rights
                .acquire(caller, request, self.issued(assessment), route, deadline)?;
        if let Err(failure) = self.recheck_caller(caller, assessment, deadline) {
            self.rights.revoke(&mut native);
            return Err(failure);
        }
        Ok(RequestBoundRight {
            native,
            request: request.clone(),
        })
    }
    fn granted_right(&self, grant: &Self::Grant) -> (BuildIdentity, OperationTag) {
        (
            grant.request.key().build_identity,
            grant.request.operation(),
        )
    }
    fn granted_request<'a>(&self, grant: &'a Self::Grant) -> &'a ManagementRequest {
        &grant.request
    }
    fn policy(&self, grant: &Self::Grant) -> Result<NativeGrantPolicy, NativeFailure> {
        self.rights.policy(&grant.native)
    }
    fn recheck(
        &self,
        caller: &Self::Caller,
        assessment: &Self::Assessment,
        grant: &Self::Grant,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        self.recheck_caller(caller, assessment, deadline)?;
        if self.request(assessment) != &grant.request {
            return Err(NativeFailure::IdentityMismatch);
        }
        self.rights
            .recheck(caller, &grant.native, &grant.request, deadline)
    }
    fn recheck_external_form(&self, _: &Self::Grant, _: &[u8; 32]) -> Result<(), NativeFailure> {
        Err(NativeFailure::AuthorizationDenied)
    }
    fn consume_external_form(
        &self,
        _: &Self::Grant,
        _: &[u8; 32],
        _: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        Err(NativeFailure::AuthorizationDenied)
    }
    fn revoke(&self, grant: &mut Self::Grant) {
        self.rights.revoke(&mut grant.native);
    }
}
