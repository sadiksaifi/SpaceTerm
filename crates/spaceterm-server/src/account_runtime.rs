//! Native lifecycle bridge on the existing paired protocol connection. A remote
//! paired connection still cannot act as the owning account's native manager.

use std::sync::{Arc, Mutex};

use spaceterm_core::{
    authority::{ServerEpoch, SessionKey},
    native_management::{NativeManagementRequestRef, NativeRetirementAction},
    retirement::{CleanupResult, ConfirmedCleanupClass, SessionCleanupObservation},
};
use spaceterm_host::account_service::{
    RegisteredServer, RuntimeAssessment, ServiceFailure, ServiceFuture, ServiceProcesses,
    ServiceSupervisor,
    runtime_inventory::{AccountRuntimeCall, AccountRuntimeReply, RuntimeCustody},
};
use spaceterm_protocol::{
    authentication::SigningKey,
    codec::Payload,
    wire::{common, v1, v2},
};

use crate::{
    api::ServerHandle,
    contracts::{ServerFailure, ServerFailureClass},
    failure::failure,
    shutdown::{StoppedAuthorizationPort, TrustedShutdownRequest},
};

type RetirementTask = (
    NativeManagementRequestRef,
    tokio::task::JoinHandle<Result<(), ServiceFailure>>,
);

/// The server actor owns Sessions and reservations. The retained native host
/// journal independently authorizes the exact retirement action and request.
pub struct AccountRuntimeProcesses {
    handle: ServerHandle,
    supervisor: ServiceSupervisor,
    custody: RuntimeCustody,
    retirements: Mutex<Vec<RetirementTask>>,
}
impl AccountRuntimeProcesses {
    /// Called once after recovery, before admitting listeners. The custody marker
    /// fences every previous clean instance even when no Session has started yet.
    pub fn new(
        handle: ServerHandle,
        supervisor: ServiceSupervisor,
        signer: Arc<dyn SigningKey>,
    ) -> Result<Arc<Self>, ServiceFailure> {
        let registration = supervisor.discover()?.ok_or(ServiceFailure::Missing)?;
        if registration.server_pin().identity != handle.snapshot().state().server.bytes() {
            return Err(ServiceFailure::IdentityMismatch);
        }
        let custody = RuntimeCustody::begin(&registration, handle.snapshot().epoch(), signer)?;
        Ok(Arc::new(Self {
            handle,
            supervisor,
            custody,
            retirements: Mutex::new(Vec::new()),
        }))
    }
    fn registration(&self) -> Result<RegisteredServer, ServiceFailure> {
        self.supervisor.discover()?.ok_or(ServiceFailure::Missing)
    }
    fn check(&self, registration: &RegisteredServer) -> Result<(), ServiceFailure> {
        let current = self.registration()?;
        if current.key() != registration.key() || current.server_pin() != registration.server_pin()
        {
            return Err(ServiceFailure::IdentityMismatch);
        }
        Ok(())
    }
    /// Dispatch only after the ordinary paired handshake and owning-account peer
    /// check. Wire comparisons cannot supply the durable native authorization.
    pub async fn dispatch(
        self: &Arc<Self>,
        payload: &Payload,
        local_uid: Option<u32>,
    ) -> Result<Option<Payload>, ServerFailure> {
        let (request, body) = match payload {
            Payload::Major1(message) => match &message.kind {
                Some(v1::message::Kind::AccountRuntimeRequest(value)) => {
                    (value.request, &value.body)
                }
                _ => return Ok(None),
            },
            Payload::Major2(message) => match &message.kind {
                Some(v2::message::Kind::AccountRuntimeRequest(value)) => {
                    (value.request, &value.body)
                }
                _ => return Ok(None),
            },
        };
        if local_uid != Some(self.supervisor.key().account_uid) {
            return Err(failure(ServerFailureClass::Authentication));
        }
        let call: AccountRuntimeCall =
            serde_json::from_slice(body).map_err(|_| failure(ServerFailureClass::ScopeMismatch))?;
        let result = self.call(call).await;
        let reply = result.unwrap_or_else(AccountRuntimeReply::Failed);
        let body = serde_json::to_vec(&reply).map_err(|_| failure(ServerFailureClass::Capacity))?;
        let message = common::AccountRuntimeMessage { request, body };
        Ok(Some(match payload {
            Payload::Major1(_) => Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::AccountRuntimeReply(message)),
            }),
            Payload::Major2(_) => Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::AccountRuntimeReply(message)),
            }),
        }))
    }
    async fn call(
        self: &Arc<Self>,
        call: AccountRuntimeCall,
    ) -> Result<AccountRuntimeReply, ServiceFailure> {
        let registration = self.registration()?;
        match call {
            AccountRuntimeCall::Assess => self
                .assess(&registration)
                .await
                .map(AccountRuntimeReply::Assessment),
            AccountRuntimeCall::Reserve(assessment) => {
                self.revalidate(&registration, &assessment).await?;
                Ok(AccountRuntimeReply::Reserved)
            }
            AccountRuntimeCall::Abort(assessment) => {
                self.abort_reservation(&registration, &assessment).await?;
                Ok(AccountRuntimeReply::Aborted)
            }
            AccountRuntimeCall::Quiesce {
                request,
                action,
                assessment,
            } => {
                self.start_retirement(registration, request, action, assessment)
                    .await?;
                Ok(AccountRuntimeReply::QuiescenceAccepted)
            }
        }
    }
    async fn start_retirement(
        self: &Arc<Self>,
        registration: RegisteredServer,
        request: NativeManagementRequestRef,
        action: NativeRetirementAction,
        assessment: RuntimeAssessment,
    ) -> Result<(), ServiceFailure> {
        self.authorize(&registration, request, action, &assessment)?;
        if self
            .retirements
            .lock()
            .map_err(|_| ServiceFailure::Indeterminate)?
            .iter()
            .any(|(old, _)| *old == request)
        {
            return Ok(());
        }
        self.revalidate(&registration, &assessment).await?;
        let mut tasks = self
            .retirements
            .lock()
            .map_err(|_| ServiceFailure::Indeterminate)?;
        if tasks.iter().any(|(old, _)| *old == request) {
            return Ok(());
        }
        if !tasks.is_empty() {
            return Err(ServiceFailure::RequestConflict);
        }
        let processes = Arc::clone(self);
        tasks.push((
            request,
            tokio::spawn(async move {
                processes
                    .quiesce(&registration, request, action, &assessment)
                    .await
                    .map(|_| ())
            }),
        ));
        Ok(())
    }
    /// The retained native journal can initiate the same original retirement when
    /// a Client disconnects before sending its paired Quiesce comparison.
    pub async fn start_original_retirement(self: &Arc<Self>) -> Result<(), ServiceFailure> {
        let registration = self.registration()?;
        let snapshot = self.handle.snapshot();
        let (request, action) = self
            .supervisor
            .verify_retirement_request(snapshot.state().server, snapshot.epoch())?;
        let assessment = self
            .supervisor
            .retirement_runtime_assessment(snapshot.state().server, snapshot.epoch())?;
        self.start_retirement(registration, request, action, assessment)
            .await
    }
    /// Production composition joins signed custody publication before releasing
    /// the native instance lease or exiting the server process.
    pub async fn finish_retirements(&self) -> Result<(), ServiceFailure> {
        let tasks = std::mem::take(
            &mut *self
                .retirements
                .lock()
                .map_err(|_| ServiceFailure::Indeterminate)?,
        );
        for (_, task) in tasks {
            task.await.map_err(|_| ServiceFailure::Indeterminate)??;
        }
        Ok(())
    }
    /// Ordinary OS-signal shutdown has no native request. Its successful report
    /// still proves joined instance cleanup without changing Running intent.
    pub fn finish_instance(
        &self,
        report: crate::shutdown::ShutdownReport,
    ) -> Result<(), ServiceFailure> {
        if !report.cleanup_confirmed
            || !report.listeners_closed
            || !report.started_writes_finished
            || report.recovery_required
        {
            return Err(ServiceFailure::CleanupUnresolved);
        }
        self.custody.completed_instance(&self.registration()?)
    }

    fn authorize(
        &self,
        registration: &RegisteredServer,
        request: NativeManagementRequestRef,
        action: NativeRetirementAction,
        assessment: &RuntimeAssessment,
    ) -> Result<(), ServiceFailure> {
        self.check(registration)?;
        let epoch = assessment.epoch.ok_or(ServiceFailure::IdentityMismatch)?;
        if assessment.server.bytes() != registration.server_pin().identity
            || self
                .supervisor
                .verify_retirement_request(assessment.server, epoch)?
                != (request, action)
        {
            return Err(ServiceFailure::RequestConflict);
        }
        Ok(())
    }
}
impl ServiceProcesses for AccountRuntimeProcesses {
    fn assess<'a>(
        &'a self,
        registration: &'a RegisteredServer,
    ) -> ServiceFuture<'a, RuntimeAssessment> {
        Box::pin(async move {
            self.check(registration)?;
            self.handle.assess_account_runtime().await.map_err(classify)
        })
    }
    fn revalidate<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        assessment: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            self.check(registration)?;
            self.handle
                .reserve_account_runtime(assessment.clone())
                .await
                .map_err(classify)
        })
    }
    fn abort_reservation<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        assessment: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, ()> {
        Box::pin(async move {
            self.check(registration)?;
            self.supervisor.verify_runtime_abort(assessment)?;
            self.handle
                .abort_account_runtime(assessment.clone())
                .await
                .map_err(classify)
        })
    }
    fn quiesce<'a>(
        &'a self,
        registration: &'a RegisteredServer,
        request: NativeManagementRequestRef,
        action: NativeRetirementAction,
        assessment: &'a RuntimeAssessment,
    ) -> ServiceFuture<'a, Vec<SessionCleanupObservation>> {
        Box::pin(async move {
            self.authorize(registration, request, action, assessment)?;
            self.handle
                .reserve_account_runtime(assessment.clone())
                .await
                .map_err(classify)?;
            let report = self
                .handle
                .shutdown(TrustedShutdownRequest {
                    authority: Arc::new(NativeRetirementAuthority {
                        supervisor: self.supervisor.clone(),
                        request,
                        action,
                    }),
                })
                .await
                .map_err(classify)?;
            if !report.cleanup_confirmed
                || !report.listeners_closed
                || !report.started_writes_finished
                || report.recovery_required
            {
                return Err(ServiceFailure::CleanupUnresolved);
            }
            let observations: Vec<_> = assessment
                .sessions
                .iter()
                .map(|session| {
                    SessionCleanupObservation::new(
                        *session,
                        action.reason(),
                        CleanupResult::Confirmed(ConfirmedCleanupClass::ReapedAndReleased),
                    )
                    .map_err(|_| ServiceFailure::InvalidRecord)
                })
                .collect::<Result<_, _>>()?;
            self.custody
                .completed(registration, request, action, assessment, &observations)?;
            Ok(observations)
        })
    }
    fn verify_absent<'a>(&'a self, _registration: &'a RegisteredServer) -> ServiceFuture<'a, ()> {
        Box::pin(async { Err(ServiceFailure::CleanupUnresolved) })
    }
    fn previous_instance_cleanup<'a>(
        &'a self,
        _registration: &'a RegisteredServer,
        _epoch: ServerEpoch,
        _sessions: &'a [SessionKey],
    ) -> ServiceFuture<'a, Vec<SessionCleanupObservation>> {
        Box::pin(async { Err(ServiceFailure::CleanupUnresolved) })
    }
}
struct NativeRetirementAuthority {
    supervisor: ServiceSupervisor,
    request: NativeManagementRequestRef,
    action: NativeRetirementAction,
}
impl StoppedAuthorizationPort for NativeRetirementAuthority {
    fn verify_stopped(
        &self,
        server: spaceterm_core::identity::ServerId,
        epoch: ServerEpoch,
    ) -> Result<(NativeManagementRequestRef, NativeRetirementAction), ServerFailure> {
        let original = self
            .supervisor
            .verify_retirement_request(server, epoch)
            .map_err(|_| failure(ServerFailureClass::NativeUnavailable))?;
        if original != (self.request, self.action) {
            return Err(failure(ServerFailureClass::ScopeMismatch));
        }
        Ok(original)
    }
}
fn classify(error: ServerFailure) -> ServiceFailure {
    match error.class {
        ServerFailureClass::StaleAuthority | ServerFailureClass::ScopeMismatch => {
            ServiceFailure::StaleAssessment
        }
        ServerFailureClass::Capacity => ServiceFailure::Pending,
        ServerFailureClass::Authentication => ServiceFailure::IdentityMismatch,
        _ => ServiceFailure::CleanupUnresolved,
    }
}
