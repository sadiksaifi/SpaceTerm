//! Fixed native route selection over the installation owner's original scopes and single journal.

use super::{
    authorization::NativeMonotonicClock,
    caller::NativeCallerAuthentication,
    entry::{ManagerInvocation, NativeManagerDependencies, NativeManagerExecution},
    operation_authority::NativeAssessmentScopes,
    transaction::{ManagerJournal, ManagerTransactionPeer},
};
use crate::native_deployment::{account::NativeAccountProvider, contracts::NativeFailure};
use crate::native_management::{
    cli_sudo::authorization::{BrokerOperationRights, PrivilegedBroker, PrivilegedManagerCustody},
    helper_lifetime::HelperDeadline,
    request::ManagementRequest,
};
use crate::startup::BuildIdentity;
use std::sync::Arc;

/// Retained installation-owner ports. This representation creates no grant, lock, or journal.
pub struct NativeManagerPorts<S, P, J> {
    scopes: S,
    accounts: Arc<dyn NativeAccountProvider>,
    clock: Arc<dyn NativeMonotonicClock>,
    transactions: P,
    journal: J,
}
impl<S, P, J> NativeManagerPorts<S, P, J> {
    /// Keeps the original assessment, native account provider and existing transaction/journal owner.
    pub fn new(
        scopes: S,
        accounts: Arc<dyn NativeAccountProvider>,
        clock: Arc<dyn NativeMonotonicClock>,
        transactions: P,
        journal: J,
    ) -> Self {
        Self {
            scopes,
            accounts,
            clock,
            transactions,
            journal,
        }
    }
}

/// Ports selected by one installation owner for its retained native caller type.
pub type RetainedManagerPorts<O, C> = NativeManagerPorts<
    <O as NativeManagerOwner<C>>::Scopes,
    <O as NativeManagerOwner<C>>::Transactions,
    <O as NativeManagerOwner<C>>::Journal,
>;

/// Native installation owner selected in trusted composition, never by a request or environment.
pub trait NativeManagerOwner<C: NativeCallerAuthentication>: Send + Sync {
    /// The installation owner's original boot/caller/request/confirmation-bound assessment.
    type Scopes: NativeAssessmentScopes<C::Caller>;
    /// Shared native serialization, consumed through the A3 port.
    type Transactions: ManagerTransactionPeer + 'static;
    /// Existing installation/lifecycle journal. No second management journal is created.
    type Journal: ManagerJournal<<Self::Transactions as ManagerTransactionPeer>::Lease> + 'static;
    /// Retains the existing owner against the actual received native caller and original request.
    fn retain(
        &self,
        callers: &C,
        request: &ManagementRequest,
        deadline: HelperDeadline,
    ) -> Result<RetainedManagerPorts<Self, C>, NativeFailure>;
}

/// Broker execution custody supplied by the same native installation owner.
pub trait NativeBrokerManagerOwner<C: NativeCallerAuthentication>: NativeManagerOwner<C> {
    /// The actual executing manager and protected sudo/pkexec route, not comparison facts.
    type Custody: PrivilegedManagerCustody<C::Caller>;
    /// Selects retained execution authority. Acquiring a right occurs inside controller authorization.
    fn custody(&self) -> Result<Self::Custody, NativeFailure>;
}

/// Production route assembly. Platform selection is fixed here; the owner supplies only its ports.
pub struct InstallationManagerAssembly<O> {
    build: BuildIdentity,
    owner: O,
}
impl<O> InstallationManagerAssembly<O> {
    /// Selects the compiled build identity and installation owner from trusted source composition.
    pub fn new(build: BuildIdentity, owner: O) -> Self {
        Self { build, owner }
    }
    /// Assembles the received sudo/pkexec route. Tests inject native peers at these same OS ports.
    pub fn assemble_broker<C: NativeCallerAuthentication>(
        &self,
        callers: Arc<C>,
        invocation: ManagerInvocation,
        request: &ManagementRequest,
        deadline: HelperDeadline,
    ) -> Result<Box<dyn NativeManagerExecution>, NativeFailure>
    where
        O: NativeBrokerManagerOwner<C>,
    {
        deadline.check()?;
        self.bind_build(request)?;
        invocation.bind(request)?;
        let broker = match invocation {
            ManagerInvocation::Sudo => PrivilegedBroker::Sudo,
            #[cfg(target_os = "linux")]
            ManagerInvocation::Polkit(operation) => PrivilegedBroker::Pkexec(operation),
            _ => return Err(NativeFailure::AuthorizationUnavailable),
        };
        let ports = self.owner.retain(&callers, request, deadline)?;
        let rights = BrokerOperationRights::new(callers.clone(), self.owner.custody()?, broker);
        deadline.check()?;
        Ok(Box::new(NativeManagerDependencies::new(
            callers,
            ports.scopes,
            rights,
            ports.accounts,
            ports.clock,
            ports.transactions,
            ports.journal,
        )))
    }
    #[cfg(target_os = "macos")]
    /// Assembles helper-local Authorization Services over the same received GUI audit sender.
    pub fn assemble_gui<
        C: crate::native_management::macos::authorization::AuthorizationServicesCaller,
    >(
        &self,
        callers: Arc<C>,
        request: &ManagementRequest,
        deadline: HelperDeadline,
    ) -> Result<Box<dyn NativeManagerExecution>, NativeFailure>
    where
        O: NativeManagerOwner<C>,
    {
        deadline.check()?;
        self.bind_build(request)?;
        let ports = self.owner.retain(&callers, request, deadline)?;
        let rights = crate::native_management::macos::authorization::AuthorizationServices::new(
            callers.clone(),
        );
        deadline.check()?;
        Ok(Box::new(NativeManagerDependencies::new(
            callers,
            ports.scopes,
            rights,
            ports.accounts,
            ports.clock,
            ports.transactions,
            ports.journal,
        )))
    }
    fn bind_build(&self, request: &ManagementRequest) -> Result<(), NativeFailure> {
        if request.key().build_identity != self.build {
            return Err(NativeFailure::IdentityMismatch);
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
impl<O: NativeBrokerManagerOwner<super::socket_caller::BrokerCallerPeer>>
    super::entry::NativeManagerAssembly for InstallationManagerAssembly<O>
{
    fn broker(
        &self,
        caller: Arc<super::socket_caller::BrokerCallerPeer>,
        invocation: ManagerInvocation,
        request: &ManagementRequest,
        deadline: HelperDeadline,
    ) -> Result<Box<dyn NativeManagerExecution>, NativeFailure> {
        self.assemble_broker(caller, invocation, request, deadline)
    }
}

#[cfg(target_os = "macos")]
impl<O> super::entry::NativeManagerAssembly for InstallationManagerAssembly<O>
where
    O: NativeBrokerManagerOwner<super::socket_caller::BrokerCallerPeer>
        + NativeManagerOwner<crate::native_management::macos::demand::MachDemandCallerPeer>,
{
    fn broker(
        &self,
        caller: Arc<super::socket_caller::BrokerCallerPeer>,
        invocation: ManagerInvocation,
        request: &ManagementRequest,
        deadline: HelperDeadline,
    ) -> Result<Box<dyn NativeManagerExecution>, NativeFailure> {
        self.assemble_broker(caller, invocation, request, deadline)
    }
    fn gui(
        &self,
        caller: Arc<crate::native_management::macos::demand::MachDemandCallerPeer>,
        request: &ManagementRequest,
        deadline: HelperDeadline,
    ) -> Result<Box<dyn NativeManagerExecution>, NativeFailure> {
        self.assemble_gui(caller, request, deadline)
    }
}
