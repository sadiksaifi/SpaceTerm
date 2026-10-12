//! One-shot sudo/pkexec authority is retained native execution, never broker exit or UID hints.

use crate::native_deployment::contracts::NativeFailure;
use crate::native_management::{
    controller::AuthorizationRoute,
    facade::{
        authorization::NativeGrantPolicy, caller::NativeCallerAuthentication,
        operation_authority::NativeOperationRights,
    },
    helper_lifetime::HelperDeadline,
    request::{ManagementRequest, OperationTag},
};
use std::{sync::Arc, time::Duration};

/// Fixed native elevation route. The privileged execution owner independently verifies it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrivilegedBroker {
    /// Protected system sudo with a fresh, nonretained authentication exchange.
    Sudo,
    /// Protected system pkexec and the exact operation-specific auth_admin action.
    Pkexec(OperationTag),
}
impl PrivilegedBroker {
    fn bind(
        self,
        request: &ManagementRequest,
        route: AuthorizationRoute,
    ) -> Result<(), NativeFailure> {
        match (self, route) {
            (
                Self::Sudo,
                AuthorizationRoute::CliInteractive | AuthorizationRoute::CliNonInteractive,
            ) => Ok(()),
            (Self::Pkexec(operation), AuthorizationRoute::Gui)
                if operation == request.operation() =>
            {
                Ok(())
            }
            (Self::Pkexec(_), AuthorizationRoute::Gui) => Err(NativeFailure::IdentityMismatch),
            _ => Err(NativeFailure::AuthorizationUnavailable),
        }
    }
}

/// Installation owner's native proof of the broker's privileged execution of this manager.
/// It retains the original kernel caller and exact broker/manager execution, not a root-UID
/// observation, environment hint, pathname, digest, exit status, or decoded request.
/// The protected publication includes both executables, dependencies, configuration, ancestors
/// and writable-alias exclusion. Supported broker policy requires authentication, excludes
/// standing passwordless rules and auth_admin_keep, and binds pkexec to the exact fixed action.
pub trait PrivilegedManagerCustody<C>: Send + Sync + 'static {
    /// Nonserializable native execution and policy authority, held through admission.
    type Lease: Send + Sync + 'static;
    /// Authenticates the actual privileged entry and the original caller for this fixed route.
    /// Native denial/cancellation cannot issue a lease. This operation never prompts again.
    fn retain_current(
        &self,
        caller: &C,
        request: &ManagementRequest,
        broker: PrivilegedBroker,
        deadline: HelperDeadline,
    ) -> Result<Self::Lease, NativeFailure>;
    /// Rechecks the retained execution, original caller, exact request, and protected policy.
    /// PKEXEC_UID or SUDO_UID alone cannot satisfy this check, and it never authenticates again.
    fn recheck(
        &self,
        lease: &Self::Lease,
        caller: &C,
        request: &ManagementRequest,
        broker: PrivilegedBroker,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure>;
}

/// Consumes one native sudo/pkexec execution. In particular, pkexec's one-shot auth_admin
/// conversation is not followed by another interactive CheckAuthorization.
pub struct BrokerOperationRights<C, P> {
    callers: Arc<C>,
    custody: P,
    broker: PrivilegedBroker,
}
impl<C: NativeCallerAuthentication, P: PrivilegedManagerCustody<C::Caller>>
    BrokerOperationRights<C, P>
{
    /// Selects the received native caller and installation owner's retained fixed execution.
    pub fn new(callers: Arc<C>, custody: P, broker: PrivilegedBroker) -> Self {
        Self {
            callers,
            custody,
            broker,
        }
    }
    fn check(
        &self,
        caller: &C::Caller,
        request: &ManagementRequest,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        self.callers.recheck(caller, deadline)?;
        if self.callers.uid(caller)? != request.key().account_uid {
            return Err(NativeFailure::IdentityMismatch);
        }
        deadline.check()
    }
}
/// One retained native execution bound to its exact request. It cannot be exported or reused.
pub struct BrokerExecutionRight<L> {
    execution: L,
    request: ManagementRequest,
}
impl<C: NativeCallerAuthentication, P: PrivilegedManagerCustody<C::Caller>>
    NativeOperationRights<C::Caller> for BrokerOperationRights<C, P>
{
    type Right = BrokerExecutionRight<P::Lease>;
    fn acquire(
        &self,
        caller: &C::Caller,
        request: &ManagementRequest,
        _: Duration,
        route: AuthorizationRoute,
        deadline: HelperDeadline,
    ) -> Result<Self::Right, NativeFailure> {
        self.broker.bind(request, route)?;
        self.check(caller, request, deadline)?;
        let execution = self
            .custody
            .retain_current(caller, request, self.broker, deadline)?;
        let right = BrokerExecutionRight {
            execution,
            request: request.clone(),
        };
        self.recheck(caller, &right, request, deadline)?;
        Ok(right)
    }
    fn policy(&self, _: &Self::Right) -> Result<NativeGrantPolicy, NativeFailure> {
        // The native custody issuer and admission rechecks require the supported password-gated policy.
        Ok(NativeGrantPolicy::PasswordAuthenticated)
    }
    fn recheck(
        &self,
        caller: &C::Caller,
        right: &Self::Right,
        request: &ManagementRequest,
        deadline: HelperDeadline,
    ) -> Result<(), NativeFailure> {
        if request != &right.request {
            return Err(NativeFailure::IdentityMismatch);
        }
        self.check(caller, request, deadline)?;
        self.custody
            .recheck(&right.execution, caller, request, self.broker, deadline)?;
        self.check(caller, request, deadline)
    }
    fn revoke(&self, _: &mut Self::Right) {
        // Unique ownership retires the lease. Neither sudo -k nor auth_admin creates a reusable grant.
    }
}
