//! Fixed pkexec transport over the same retained manager/broker custody as sudo.

use crate::native_deployment::contracts::{NativeFailure, NativeManagementFuture};
use crate::native_management::{
    cli_sudo::{
        execution::invoke_broker,
        protected_execution::{
            ManagementBroker, NativeManagementExecutionCustody, ProtectedManagementExecution,
        },
    },
    controller::ManagementProgress,
    facade::{ConfirmedNativeAction, transport::NativeGuiManagementTransport},
    request::ManagementRequest,
};
use std::sync::Arc;

/// Native GUI transport with no AskPass, internal agent, password input or caller-selected path.
pub struct NativePkexecManagement<P: NativeManagementExecutionCustody> {
    execution: Arc<ProtectedManagementExecution<P>>,
}
impl<P: NativeManagementExecutionCustody> NativePkexecManagement<P> {
    /// Shares the native publication owner's retained execution provider with CLI management.
    pub fn new(execution: ProtectedManagementExecution<P>) -> Self {
        Self {
            execution: Arc::new(execution),
        }
    }
    /// Executes the exact assessed request. Success requires the authenticated manager's bounded reply.
    pub fn execute_request(
        &self,
        request: &ManagementRequest,
    ) -> Result<ManagementProgress, NativeFailure> {
        Self::run(&self.execution, request)
    }
    fn run(
        execution: &ProtectedManagementExecution<P>,
        request: &ManagementRequest,
    ) -> Result<ManagementProgress, NativeFailure> {
        let mut lease = execution.retain_pkexec(request)?;
        let command = execution.pkexec(&mut lease, request)?;
        let progress = invoke_broker(
            command,
            lease,
            &request.encode(),
            ManagementBroker::Pkexec,
            |lease| execution.recheck(lease),
        )?;
        if progress.request != *request.request_bytes() {
            return Err(NativeFailure::IdentityMismatch);
        }
        Ok(progress)
    }
}
impl<P: NativeManagementExecutionCustody> NativeGuiManagementTransport
    for NativePkexecManagement<P>
{
    fn execute(
        &self,
        confirmed: ConfirmedNativeAction,
    ) -> NativeManagementFuture<'_, ManagementProgress> {
        Box::pin(async move {
            let request = confirmed.assessment().request().clone();
            let execution = Arc::clone(&self.execution);
            let (reply, waiting) = tokio::sync::oneshot::channel();
            // Client cancellation drops only the wait; native custody and exact reaping continue.
            std::thread::Builder::new()
                .name("native-pkexec-management".into())
                .spawn(move || {
                    let _ = reply.send(Self::run(&execution, &request));
                })
                .map_err(|_| NativeFailure::NativeUnavailable)?;
            waiting
                .await
                .map_err(|_| NativeFailure::NativeUnavailable)?
        })
    }
}
