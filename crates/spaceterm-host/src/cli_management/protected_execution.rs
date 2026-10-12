//! Fixed broker commands over the native publisher's retained execution closure.

use super::{RetainedSudoExecution, SudoInvocation};
use crate::native_deployment::contracts::NativeFailure;
use crate::native_management::{linux::PkexecInvocation, request::ManagementRequest};
use std::{path::Path, process::Command};

/// Closed OS broker selection, never caller text or an executable pathname.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManagementBroker {
    /// Fixed system sudo.
    Sudo,
    /// Fixed system pkexec.
    Pkexec,
}
/// The installation owner's retained, authenticated broker and manager closure.
/// Implementations must retain protected code, dependencies, configuration, ancestors,
/// mount evidence and publication policy excluding writable aliases through exact reaping.
pub trait NativeManagementExecutionCustody: Send + Sync + 'static {
    /// Noncloneable native execution authority. No constructor accepts comparison facts.
    type Lease: Send + Sync + 'static;
    /// Retains the fixed system broker and registered approved manager for this account/build.
    fn retain(
        &self,
        request: &ManagementRequest,
        broker: ManagementBroker,
    ) -> Result<Self::Lease, NativeFailure>;
    /// Manager name from the retained protected namespace, never client or environment input.
    fn manager<'a>(&self, lease: &'a Self::Lease) -> &'a Path;
    /// Rechecks actual custody and exact destination authentication.
    fn recheck(&self, lease: &mut Self::Lease) -> Result<(), NativeFailure>;
    /// Installs the retained broker's descriptor/exec binding. The fixed manager namespace
    /// stays protected and replacement-excluded while the lease is held.
    fn bind_exec(
        &self,
        lease: &mut Self::Lease,
        broker: ManagementBroker,
        command: &mut Command,
    ) -> Result<(), NativeFailure>;
}
/// Concrete fixed command construction. Native publication remains the authority issuer.
pub struct ProtectedManagementExecution<P> {
    custody: P,
}
impl<P: NativeManagementExecutionCustody> ProtectedManagementExecution<P> {
    /// Selects the native owner by constructor injection, not paths or readiness facts.
    pub fn new(custody: P) -> Self {
        Self { custody }
    }
    pub(crate) fn retain_pkexec(
        &self,
        request: &ManagementRequest,
    ) -> Result<P::Lease, NativeFailure> {
        self.custody.retain(request, ManagementBroker::Pkexec)
    }
    pub(crate) fn pkexec(
        &self,
        lease: &mut P::Lease,
        request: &ManagementRequest,
    ) -> Result<Command, NativeFailure> {
        self.custody.recheck(lease)?;
        let invocation = PkexecInvocation::for_request(request);
        let mut command = Command::new(invocation.executable());
        command
            .arg("--disable-internal-agent")
            .arg(self.custody.manager(lease))
            .args(invocation.manager_arguments());
        self.custody
            .bind_exec(lease, ManagementBroker::Pkexec, &mut command)?;
        self.custody.recheck(lease)?;
        Ok(command)
    }
    pub(crate) fn recheck(&self, lease: &mut P::Lease) -> Result<(), NativeFailure> {
        self.custody.recheck(lease)
    }
}
impl<P: NativeManagementExecutionCustody> RetainedSudoExecution
    for ProtectedManagementExecution<P>
{
    type Helper = P::Lease;
    fn retain(&self, request: &ManagementRequest) -> Result<Self::Helper, NativeFailure> {
        self.custody.retain(request, ManagementBroker::Sudo)
    }
    fn recheck(&self, helper: &mut Self::Helper) -> Result<(), NativeFailure> {
        self.custody.recheck(helper)
    }
    fn prepare(
        &self,
        helper: &mut Self::Helper,
        invocation: SudoInvocation,
    ) -> Result<Command, NativeFailure> {
        self.custody.recheck(helper)?;
        let mut command = Command::new(invocation.executable());
        command
            .args(invocation.prefix())
            .arg(self.custody.manager(helper))
            .args(invocation.manager_arguments());
        self.custody
            .bind_exec(helper, ManagementBroker::Sudo, &mut command)?;
        self.custody.recheck(helper)?;
        Ok(command)
    }
}
