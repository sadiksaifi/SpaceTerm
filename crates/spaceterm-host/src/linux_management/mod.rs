//! Fixed Linux GUI management invocation facts. They do not authorize execution.

use crate::native_deployment::contracts::NativeFailure;
use crate::native_management::request::{ManagementRequest, OperationTag};
use crate::startup::BuildIdentity;

/// Bounded pkexec transport plan. The caller cannot select executable authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PkexecInvocation {
    operation: OperationTag,
}
impl PkexecInvocation {
    /// Derive only a fixed operation argument from a strictly decoded request.
    pub fn for_request(request: &ManagementRequest) -> Self {
        Self {
            operation: request.operation(),
        }
    }
    /// Fixed OS program. Native execution must retain and authenticate its closure.
    pub fn executable(self) -> &'static str {
        "/usr/bin/pkexec"
    }
    /// First manager argument selects the operation-specific polkit exec.argv1 action.
    /// Deliver the request in a private stdin pipe, never descriptor 3 or above.
    /// The manager must claim RequestStdin before any subsequent child and compare it to the privately delivered request's operation.
    pub fn manager_arguments(self) -> [&'static str; 2] {
        [self.operation.right_suffix(), "--request-stdin"]
    }
    /// Exact action, locally derived rather than accepted from request text.
    pub fn action(self, build: BuildIdentity) -> String {
        crate::native_management::request::authorization_right(build, self.operation)
    }
}

/// Process exit alone cannot distinguish no agent, denial, exec failure, and
/// cancelled policy requests. Read the native polkit outcome through its bound
/// subject/provider. Neither PKEXEC_UID nor a process exit creates caller proof.
pub fn authorize_native_request<
    P: crate::native_management::facade::authorization::NativeAuthorizationBoundary,
>(
    issuer: &crate::native_management::facade::authorization::NativeAuthorizationIssuer<P>,
    request: &ManagementRequest,
    deadline: crate::native_management::helper_lifetime::HelperDeadline,
) -> Result<crate::native_management::facade::authorization::NativeAuthorizationLease, NativeFailure>
{
    issuer.authorize(
        request,
        crate::native_management::controller::AuthorizationRoute::Gui,
        deadline,
    )
}

#[path = "execution.rs"]
pub mod execution;
#[path = "polkit.rs"]
pub mod polkit;
#[cfg(target_os = "linux")]
#[path = "system_polkit.rs"]
pub mod system_polkit;
