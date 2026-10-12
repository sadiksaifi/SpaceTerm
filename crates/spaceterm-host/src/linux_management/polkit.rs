//! Authenticated polkit subject and typed native outcome, separate from pkexec's exit and environment.

use crate::native_deployment::contracts::NativeFailure;
use crate::native_management::{
    controller::AuthorizationRoute,
    facade::authorization::{NativeAuthorizationBoundary, NativeGrantPolicy},
    helper_lifetime::HelperDeadline,
    request::{ManagementRequest, OperationTag, authorization_right},
};
use crate::startup::BuildIdentity;
use std::time::Duration;

/// Native authority response. Neither a pkexec exit code nor PKEXEC_UID creates this response.
pub enum PolkitNativeOutcome<G> {
    /// Retained exact subject/action grant obtained from the native authority.
    Granted(G),
    /// Native authority denied this action.
    Denied,
    /// The native authentication conversation was cancelled.
    Cancelled,
    /// No authentication agent is available for the authenticated subject.
    AgentUnavailable,
    /// Native subject, authority, or policy observation is unavailable.
    Unavailable,
}
impl<G> PolkitNativeOutcome<G> {
    fn granted(self) -> Result<G, NativeFailure> {
        match self {
            Self::Granted(grant) => Ok(grant),
            Self::Denied => Err(NativeFailure::AuthorizationDenied),
            Self::Cancelled => Err(NativeFailure::AuthorizationCancelled),
            Self::AgentUnavailable | Self::Unavailable => {
                Err(NativeFailure::AuthorizationUnavailable)
            }
        }
    }
}

/// Native system-bus/process subject and exact polkit action authority.
/// The provider retains authenticated sender identity and supported protected policy.
/// Subject constructors from request UID, PKEXEC_UID, argv, or broker exit are forbidden.
pub trait NativePolkitPeer: Send + Sync + 'static {
    /// Actual authenticated unique-bus sender or retained process incarnation.
    type Subject: Send + Sync + 'static;
    /// Retained operation-specific native authorization handle.
    type Grant: Send + Sync + 'static;
    /// Authenticates the actual native sender without prompting or consulting request fields.
    fn subject(&self, deadline: HelperDeadline) -> Result<Self::Subject, NativeFailure>;
    /// Effective UID from the actual retained native subject.
    fn uid(&self, subject: &Self::Subject) -> Result<u32, NativeFailure>;
    /// Rechecks the same sender incarnation, never a reopened PID or a reused bus name.
    fn recheck_subject(
        &self,
        subject: &Self::Subject,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure>;
    /// Verifies native authority for the original enrolled request and exact locally derived action.
    fn authorize(
        &self,
        subject: &Self::Subject,
        request: &ManagementRequest,
        action: &str,
        deadline: HelperDeadline,
    ) -> PolkitNativeOutcome<Self::Grant>;
    /// Observes the supported effective policy. Password-authenticated temporary caches are allowed.
    fn policy(&self, grant: &Self::Grant) -> Result<NativeGrantPolicy, NativeFailure>;
    /// Verifies the retained subject/action/request binding through admission without another prompt.
    fn recheck_grant(
        &self,
        subject: &Self::Subject,
        grant: &Self::Grant,
        request: &ManagementRequest,
        action: &str,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure>;
    /// Retires the owned exact grant on expiry, cancellation, failed admission, or scope retirement.
    fn revoke(&self, grant: &mut Self::Grant);
}

/// Actual native subject/action plus the installation owner's protected original scope.
pub struct PolkitAuthorization<P, S> {
    native: P,
    scopes: S,
}
impl<P: NativePolkitPeer, S: NativeAuthorizationBoundary<Caller = P::Subject>>
    PolkitAuthorization<P, S>
{
    /// Injects native polkit authority and the existing protected scope, not subject facts.
    pub fn new(native: P, scopes: S) -> Self {
        Self { native, scopes }
    }
}
/// Nonserializable native polkit grant and exact protected enrollment.
pub struct EnrolledPolkitGrant<P: NativePolkitPeer, S: NativeAuthorizationBoundary> {
    native: P::Grant,
    enrollment: S::Grant,
}
impl<P: NativePolkitPeer, S: NativeAuthorizationBoundary<Caller = P::Subject>>
    NativeAuthorizationBoundary for PolkitAuthorization<P, S>
{
    type Caller = P::Subject;
    type Assessment = S::Assessment;
    type Grant = EnrolledPolkitGrant<P, S>;
    fn bind_caller(
        &self,
        route: AuthorizationRoute,
        deadline: HelperDeadline,
    ) -> Result<Self::Caller, NativeFailure> {
        if route != AuthorizationRoute::Gui {
            return Err(NativeFailure::AuthorizationUnavailable);
        }
        self.native.subject(deadline)
    }
    fn retain_assessment(
        &self,
        caller: &Self::Caller,
        request: &ManagementRequest,
        deadline: HelperDeadline,
    ) -> Result<Self::Assessment, NativeFailure> {
        self.native.recheck_subject(caller, deadline)?;
        if self.native.uid(caller)? != request.key().account_uid {
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
        self.native.recheck_subject(caller, deadline)?;
        if self.native.uid(caller)? != self.request(assessment).key().account_uid {
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
        let request = self.request(assessment);
        let action = authorization_right(request.key().build_identity, request.operation());
        let mut native = self
            .native
            .authorize(caller, request, &action, deadline)
            .granted()?;
        match self
            .scopes
            .verify_right(caller, assessment, route, deadline)
        {
            Ok(enrollment) => Ok(EnrolledPolkitGrant { native, enrollment }),
            Err(failure) => {
                self.native.revoke(&mut native);
                Err(failure)
            }
        }
    }
    fn granted_right(&self, grant: &Self::Grant) -> (BuildIdentity, OperationTag) {
        self.scopes.granted_right(&grant.enrollment)
    }
    fn granted_request<'a>(&self, grant: &'a Self::Grant) -> &'a ManagementRequest {
        self.scopes.granted_request(&grant.enrollment)
    }
    fn policy(&self, grant: &Self::Grant) -> Result<NativeGrantPolicy, NativeFailure> {
        let native = self.native.policy(&grant.native)?;
        let enrolled = self.scopes.policy(&grant.enrollment)?;
        for policy in [native, enrolled] {
            if matches!(
                policy,
                NativeGrantPolicy::StandingPasswordlessPolkit
                    | NativeGrantPolicy::StandingPasswordlessSudo
            ) {
                return Ok(policy);
            }
        }
        Ok(native)
    }
    fn recheck(
        &self,
        caller: &Self::Caller,
        assessment: &Self::Assessment,
        grant: &Self::Grant,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        self.recheck_caller(caller, assessment, deadline)?;
        let request = self.request(assessment);
        let action = authorization_right(request.key().build_identity, request.operation());
        self.native
            .recheck_grant(caller, &grant.native, request, &action, deadline)?;
        self.scopes
            .recheck(caller, assessment, &grant.enrollment, deadline)
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
        self.native.revoke(&mut grant.native);
        self.scopes.revoke(&mut grant.enrollment);
    }
}
