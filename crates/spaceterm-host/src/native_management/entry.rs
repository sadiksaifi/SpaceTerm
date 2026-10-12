//! One-request manager entry, with the lifetime guard installed before transport or output.

use crate::native_deployment::{
    contracts::NativeFailure, production_gate::require_native_evidence,
};
use crate::native_management::{
    controller::{AuthorizationRoute, ManagementBackend, ManagementController, ManagementProgress},
    helper_lifetime::{HelperDeadline, HelperLifetimeGuard},
    request::{ManagementRequest, OperationTag},
    stdin::RequestStdin,
};
use std::{
    ffi::{OsStr, OsString},
    io::Write,
};

/// Fixed manager invocation. Arguments grant neither caller nor account authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagerInvocation {
    /// Sudo stdin transport. Authentication remains in the OS controlling terminal.
    Sudo,
    /// Fixed pkexec operation and stdin transport.
    Polkit(OperationTag),
    /// launchd's fixed demand endpoint; audit-token transport is required.
    GuiDemand,
}
impl ManagerInvocation {
    /// Recognizes only the frozen invocations and rejects trailing arguments.
    pub fn parse(arguments: &[OsString]) -> Result<Self, NativeFailure> {
        match arguments {
            [argument] if argument == OsStr::new("--request-stdin") => Ok(Self::Sudo),
            [argument] if argument == OsStr::new("--gui-demand") => Ok(Self::GuiDemand),
            [operation, transport] if transport == OsStr::new("--request-stdin") => {
                for tag in 1..=11 {
                    let tag = OperationTag::decode(tag)?;
                    if operation == OsStr::new(tag.right_suffix()) {
                        return Ok(Self::Polkit(tag));
                    }
                }
                Err(NativeFailure::InvalidRecord)
            }
            _ => Err(NativeFailure::InvalidRecord),
        }
    }
    /// Checks operation substitution before calling any authorization or effect adapter.
    pub fn bind(self, request: &ManagementRequest) -> Result<AuthorizationRoute, NativeFailure> {
        match self {
            Self::Sudo => Ok(AuthorizationRoute::CliInteractive),
            Self::Polkit(operation) if operation == request.operation() => {
                Ok(AuthorizationRoute::Gui)
            }
            Self::Polkit(_) => Err(NativeFailure::IdentityMismatch),
            Self::GuiDemand => Ok(AuthorizationRoute::Gui),
        }
    }
}

/// Runs an actual controller through constructor-injected native boundaries.
/// The caller installs the independent guard before collecting args or reading transport.
/// Output executes while the same guard is still alive; blocked output cannot extend lifetime.
pub fn dispatch<B: ManagementBackend>(
    backend: B,
    lifetime: HelperLifetimeGuard,
    invocation: ManagerInvocation,
    request: ManagementRequest,
    output: impl FnOnce(Result<ManagementProgress, NativeFailure>, HelperDeadline),
) {
    let deadline = lifetime.deadline();
    let controller = ManagementController::new(backend, lifetime);
    let result = invocation
        .bind(&request)
        .and_then(|route| controller.execute(&request, route));
    output(result, deadline);
}

/// Production process entry. The reviewed source gate remains closed in this seed.
/// Native transaction/account and native caller composition must replace the seed after integration;
/// a runtime option, external form, environment, or request can never bypass this gate.
pub fn production(arguments: &[OsString], lifetime: HelperLifetimeGuard) -> u8 {
    let deadline = lifetime.deadline();
    let result = (|| {
        deadline.check()?;
        let invocation = ManagerInvocation::parse(arguments)?;
        if invocation == ManagerInvocation::GuiDemand {
            require_native_evidence()?;
            return Err(NativeFailure::AuthorizationUnavailable);
        }
        let stdin = RequestStdin::claim()?;
        let request = if stdin.is_socket() {
            // The source gate precedes even the challenge in the native principal route.
            require_native_evidence()?;
            let (request, _caller) = stdin.read_authenticated(deadline)?;
            request
        } else {
            // Legacy nonauthorizing pipe recognition stays available for bounded refusal.
            stdin.read(deadline)?
        };
        invocation.bind(&request)?;
        require_native_evidence()?;
        // The committed seed cannot inhabit AuthorizedNativeRequest or NativeTransactionLease.
        // Do not manufacture either from decoded request/account facts.
        Err(NativeFailure::AuthorizationUnavailable)
    })();
    let failure = match result {
        Err(failure) => failure,
        Ok(()) => NativeFailure::UnsafeNativeState,
    };
    let code = write_failure(failure, deadline);
    drop(lifetime);
    code
}

/// Writes one bounded classification, never raw broker output or native errors.
pub fn write_failure(failure: NativeFailure, deadline: HelperDeadline) -> u8 {
    if deadline.check().is_err() {
        return NativeFailure::TimedOut as u8;
    }
    let output = format!("classification={}\n", failure as u32);
    if std::io::stdout()
        .lock()
        .write_all(output.as_bytes())
        .is_err()
    {
        return NativeFailure::IoUnavailable as u8;
    }
    failure as u8
}
