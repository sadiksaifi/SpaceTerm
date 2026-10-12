//! OS principal binding separate from the installation owner's protected original scope.

use super::authorization::{NativeAuthorizationBoundary, NativeGrantPolicy};
use crate::native_deployment::contracts::NativeFailure;
use crate::native_management::request::OperationTag;
use crate::native_management::{
    controller::AuthorizationRoute, helper_lifetime::HelperDeadline, request::ManagementRequest,
};
use crate::startup::BuildIdentity;
use std::time::Duration;

/// Actual OS message/broker principal, never a request UID or environment field.
pub trait NativeCallerAuthentication: Send + Sync + 'static {
    /// Retained OS audit/process/polkit sender handle.
    type Caller: Send + Sync + 'static;
    /// Authenticates the actual invoking native sender for the fixed route.
    fn bind(
        &self,
        route: AuthorizationRoute,
        deadline: HelperDeadline,
    ) -> Result<Self::Caller, NativeFailure>;
    /// Effective account observed from the retained native sender, not a client assertion.
    fn uid(&self, caller: &Self::Caller) -> Result<u32, NativeFailure>;
    /// Rechecks the original retained sender incarnation.
    fn recheck(&self, caller: &Self::Caller, deadline: HelperDeadline)
    -> Result<(), NativeFailure>;
}

/// Concrete principal-to-original-scope adapter. The native owner still issues every grant.
pub struct CallerBoundAuthorization<C, S> {
    callers: C,
    scopes: S,
}
impl<C: NativeCallerAuthentication, S: NativeAuthorizationBoundary<Caller = C::Caller>>
    CallerBoundAuthorization<C, S>
{
    /// Injects actual native caller observation and the installation owner's protected original scope.
    pub fn new(callers: C, scopes: S) -> Self {
        Self { callers, scopes }
    }
}
impl<C: NativeCallerAuthentication, S: NativeAuthorizationBoundary<Caller = C::Caller>>
    NativeAuthorizationBoundary for CallerBoundAuthorization<C, S>
{
    type Caller = C::Caller;
    type Assessment = S::Assessment;
    type Grant = S::Grant;
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
        self.scopes.retain_assessment(caller, request, deadline)
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
        if self.callers.uid(caller)? != self.scopes.request(assessment).key().account_uid {
            return Err(NativeFailure::IdentityMismatch);
        }
        self.scopes.recheck_caller(caller, assessment, deadline)
    }
    fn verify_right(
        &self,
        caller: &Self::Caller,
        assessment: &Self::Assessment,
        route: AuthorizationRoute,
        deadline: HelperDeadline,
    ) -> Result<Self::Grant, NativeFailure> {
        self.recheck_caller(caller, assessment, deadline)?;
        self.scopes
            .verify_right(caller, assessment, route, deadline)
    }
    fn granted_right(&self, grant: &Self::Grant) -> (BuildIdentity, OperationTag) {
        self.scopes.granted_right(grant)
    }
    fn granted_request<'a>(&self, grant: &'a Self::Grant) -> &'a ManagementRequest {
        self.scopes.granted_request(grant)
    }
    fn policy(&self, grant: &Self::Grant) -> Result<NativeGrantPolicy, NativeFailure> {
        self.scopes.policy(grant)
    }
    fn recheck(
        &self,
        caller: &Self::Caller,
        assessment: &Self::Assessment,
        grant: &Self::Grant,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        self.recheck_caller(caller, assessment, deadline)?;
        self.scopes.recheck(caller, assessment, grant, deadline)
    }
    fn recheck_external_form(
        &self,
        grant: &Self::Grant,
        external: &[u8; 32],
    ) -> Result<(), NativeFailure> {
        self.scopes.recheck_external_form(grant, external)
    }
    fn consume_external_form(
        &self,
        grant: &Self::Grant,
        external: &[u8; 32],
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        self.scopes.consume_external_form(grant, external, deadline)
    }
    fn revoke(&self, grant: &mut Self::Grant) {
        self.scopes.revoke(grant);
    }
}

impl<T: NativeCallerAuthentication> NativeCallerAuthentication for std::sync::Arc<T> {
    type Caller = T::Caller;
    fn bind(
        &self,
        route: AuthorizationRoute,
        deadline: HelperDeadline,
    ) -> Result<Self::Caller, NativeFailure> {
        (**self).bind(route, deadline)
    }
    fn uid(&self, caller: &Self::Caller) -> Result<u32, NativeFailure> {
        (**self).uid(caller)
    }
    fn recheck(
        &self,
        caller: &Self::Caller,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        (**self).recheck(caller, deadline)
    }
}
