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

/// Native management composition. The original-scope and journal ports belong to
/// the installation owner; this entry neither fabricates authority nor stores a journal.
pub struct NativeManagerDependencies<C, S, R, P, J> {
    callers: C,
    scopes: S,
    rights: R,
    accounts: std::sync::Arc<dyn crate::native_deployment::account::NativeAccountProvider>,
    clock: std::sync::Arc<dyn super::authorization::NativeMonotonicClock>,
    transactions: P,
    journal: J,
}
impl<C, S, R, P, J> NativeManagerDependencies<C, S, R, P, J>
where
    C: super::caller::NativeCallerAuthentication,
    S: super::operation_authority::NativeAssessmentScopes<C::Caller>,
    R: super::operation_authority::NativeOperationRights<C::Caller>,
    P: super::transaction::ManagerTransactionPeer,
    J: super::transaction::ManagerJournal<P::Lease>,
{
    /// Selects native boundaries and the installation owner's existing transaction and journal.
    pub fn new(
        callers: C,
        scopes: S,
        rights: R,
        accounts: std::sync::Arc<dyn crate::native_deployment::account::NativeAccountProvider>,
        clock: std::sync::Arc<dyn super::authorization::NativeMonotonicClock>,
        transactions: P,
        journal: J,
    ) -> Self {
        Self {
            callers,
            scopes,
            rights,
            accounts,
            clock,
            transactions,
            journal,
        }
    }
    /// Assembles the actual issuer, native transaction adapter and management controller.
    /// Production calls this only after the source gate and authenticated native request reception.
    pub fn dispatch(
        self,
        lifetime: HelperLifetimeGuard,
        invocation: ManagerInvocation,
        request: ManagementRequest,
        output: impl FnOnce(Result<ManagementProgress, NativeFailure>, HelperDeadline),
    ) {
        let boundary = std::sync::Arc::new(
            super::operation_authority::ScopedManagementAuthorization::new(
                self.callers,
                self.scopes,
                self.rights,
            ),
        );
        let issuer = super::authorization::NativeAuthorizationIssuer::new(
            boundary,
            self.accounts,
            self.clock,
        );
        let owner =
            super::transaction::TransactionBackedManager::new(self.transactions, self.journal);
        dispatch(
            super::backend::NativeManagerBackend::new(issuer, owner),
            lifetime,
            invocation,
            request,
            output,
        );
    }
}

/// A fully assembled controller. Its concrete implementation owns the original native ports.
/// The output callback runs before its original lifetime guard is released.
pub trait NativeManagerExecution {
    /// Consumes one assembled manager and executes only the received invocation and request.
    fn dispatch(
        self: Box<Self>,
        lifetime: HelperLifetimeGuard,
        invocation: ManagerInvocation,
        request: ManagementRequest,
        output: &mut dyn FnMut(Result<ManagementProgress, NativeFailure>, HelperDeadline),
    );
}
impl<C, S, R, P, J> NativeManagerExecution for NativeManagerDependencies<C, S, R, P, J>
where
    C: super::caller::NativeCallerAuthentication,
    S: super::operation_authority::NativeAssessmentScopes<C::Caller>,
    R: super::operation_authority::NativeOperationRights<C::Caller>,
    P: super::transaction::ManagerTransactionPeer,
    J: super::transaction::ManagerJournal<P::Lease>,
{
    fn dispatch(
        self: Box<Self>,
        lifetime: HelperLifetimeGuard,
        invocation: ManagerInvocation,
        request: ManagementRequest,
        output: &mut dyn FnMut(Result<ManagementProgress, NativeFailure>, HelperDeadline),
    ) {
        (*self).dispatch(lifetime, invocation, request, output);
    }
}

/// Installation-owner composition at the manager entry. It selects the shared transaction,
/// original assessment, account provider and journal, never a second management journal.
/// Implementations use NativeManagerDependencies with the actual received caller adapter.
pub trait NativeManagerAssembly {
    /// Assembles fixed sudo or operation-specific polkit rights over this authenticated writer.
    fn broker(
        &self,
        caller: std::sync::Arc<super::socket_caller::BrokerCallerPeer>,
        invocation: ManagerInvocation,
        request: &ManagementRequest,
        deadline: HelperDeadline,
    ) -> Result<Box<dyn NativeManagerExecution>, NativeFailure>;
    /// Assembles helper-local Authorization Services over this kernel-audited GUI sender.
    #[cfg(target_os = "macos")]
    fn gui(
        &self,
        caller: std::sync::Arc<crate::native_management::macos::demand::MachDemandCallerPeer>,
        request: &ManagementRequest,
        deadline: HelperDeadline,
    ) -> Result<Box<dyn NativeManagerExecution>, NativeFailure>;
}

struct UnavailableInstallationAssembly;
impl NativeManagerAssembly for UnavailableInstallationAssembly {
    fn broker(
        &self,
        _: std::sync::Arc<super::socket_caller::BrokerCallerPeer>,
        _: ManagerInvocation,
        _: &ManagementRequest,
        _: HelperDeadline,
    ) -> Result<Box<dyn NativeManagerExecution>, NativeFailure> {
        Err(NativeFailure::CustodyUnavailable)
    }
    #[cfg(target_os = "macos")]
    fn gui(
        &self,
        _: std::sync::Arc<crate::native_management::macos::demand::MachDemandCallerPeer>,
        _: &ManagementRequest,
        _: HelperDeadline,
    ) -> Result<Box<dyn NativeManagerExecution>, NativeFailure> {
        Err(NativeFailure::CustodyUnavailable)
    }
}

/// Default entry remains fail-closed until the installation owner's native assembly is linked.
/// The coordinator replaces only this owner selection after A3 and C2 integrate.
pub fn production(arguments: &[OsString], lifetime: HelperLifetimeGuard) -> u8 {
    production_with_assembly(
        arguments,
        lifetime,
        compiled_build(),
        &UnavailableInstallationAssembly,
    )
}

fn compiled_build() -> crate::startup::BuildIdentity {
    match option_env!("SPACETERM_SERVER_BUILD") {
        Some("release") => crate::startup::BuildIdentity::Release,
        Some("preflight") => crate::startup::BuildIdentity::Preflight,
        _ => crate::startup::BuildIdentity::Development,
    }
}

/// Selects the fixed platform assembly over the installation owner's native ports.
/// The coordinator's production owner supplies A3/C2 ports; tests inject their native peers.
/// This entry preserves the source gate and derives the helper build from compiled identity.
pub fn production_with_owner<O>(
    arguments: &[OsString],
    lifetime: HelperLifetimeGuard,
    owner: O,
) -> u8
where
    super::assembly::InstallationManagerAssembly<O>: NativeManagerAssembly,
{
    let build = compiled_build();
    production_with_assembly(
        arguments,
        lifetime,
        build,
        &super::assembly::InstallationManagerAssembly::new(build, owner),
    )
}

/// Source-gated production entry with an installation-owner assembly selected by trusted code.
/// No feature, environment value or request selects an owner or bypasses the evidence gate.
/// The compiled helper identity is independent of the incoming request's build namespace.
pub fn production_with_assembly(
    arguments: &[OsString],
    lifetime: HelperLifetimeGuard,
    build: crate::startup::BuildIdentity,
    assembly: &impl NativeManagerAssembly,
) -> u8 {
    let deadline = lifetime.deadline();
    let invocation = match ManagerInvocation::parse(arguments).and_then(|invocation| {
        deadline.check()?;
        require_native_evidence()?;
        Ok(invocation)
    }) {
        Ok(invocation) => invocation,
        Err(failure) => return write_failure(failure, deadline),
    };
    #[cfg(target_os = "macos")]
    if invocation == ManagerInvocation::GuiDemand {
        use crate::native_management::macos::demand::MachDemandCallerPeer;
        let (caller, request) = match MachDemandCallerPeer::receive(build, deadline) {
            Ok(received) => received,
            Err(failure) => return write_failure(failure, deadline),
        };
        let caller = std::sync::Arc::new(caller);
        let manager = match bind_build(&request, build)
            .and_then(|()| assembly.gui(caller.clone(), &request, deadline))
        {
            Ok(manager) => manager,
            Err(failure) => {
                return match caller.reply(Err(failure), deadline) {
                    Ok(()) => failure as u8,
                    Err(reply_failure) => reply_failure as u8,
                };
            }
        };
        let mut code = NativeFailure::UnsafeNativeState as u8;
        manager.dispatch(lifetime, invocation, request, &mut |result, deadline| {
            code = match caller.reply(result, deadline) {
                Ok(()) => result.err().map_or(0, |failure| failure as u8),
                Err(failure) => failure as u8,
            };
        });
        return code;
    }
    if invocation == ManagerInvocation::GuiDemand {
        return write_failure(NativeFailure::AuthorizationUnavailable, deadline);
    }
    let received = (|| {
        let stdin = RequestStdin::claim()?;
        // A legacy FIFO cannot prove its writer and never enters native assembly.
        if !stdin.is_socket() {
            return Err(NativeFailure::AuthorizationUnavailable);
        }
        let (request, caller) = stdin.read_authenticated(deadline)?;
        invocation.bind(&request)?;
        bind_build(&request, build)?;
        let manager =
            assembly.broker(std::sync::Arc::new(caller), invocation, &request, deadline)?;
        Ok((request, manager))
    })();
    let (request, manager) = match received {
        Ok(received) => received,
        Err(failure) => return write_failure(failure, deadline),
    };
    let mut code = NativeFailure::UnsafeNativeState as u8;
    manager.dispatch(lifetime, invocation, request, &mut |result, deadline| {
        code = write_result(result, deadline);
    });
    code
}

fn bind_build(
    request: &ManagementRequest,
    build: crate::startup::BuildIdentity,
) -> Result<(), NativeFailure> {
    if request.key().build_identity != build {
        return Err(NativeFailure::IdentityMismatch);
    }
    Ok(())
}

fn write_result(result: Result<ManagementProgress, NativeFailure>, deadline: HelperDeadline) -> u8 {
    let progress = match result {
        Ok(progress) => progress,
        Err(failure) => return write_failure(failure, deadline),
    };
    if deadline.check().is_err() {
        return NativeFailure::TimedOut as u8;
    }
    let bytes = crate::native_management::cli_sudo::reply::encode(progress);
    match std::io::stdout().lock().write_all(bytes.as_bytes()) {
        Ok(()) => 0,
        Err(_) => NativeFailure::IoUnavailable as u8,
    }
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
