//! Durable local suppression, exact deferred releases and the sole restricted
//! self-revocation retry scheduler. No pending task can restore ordinary use.

mod scheduler;
pub use scheduler::PendingRuntime;

use crate::{
    api::PairedServerBinding,
    failure::{ClientFailure, ClientFailureClass, invalid_peer},
    journal::{
        ClientJournalChange, DetachTask, JournalController, JournalReceipt, JournalRevocation,
        RevocationPhase, SavedTarget,
    },
    operations::{OperationPeer, OperationTracker},
    ports::ClientFuture,
};
use spaceterm_core::{
    authority::GrantedControl,
    control::ReleaseControlRequest,
    identity::WorkspaceId,
    operation_ledger_contracts::CanonicalOperationRequest,
    operations::{Command, OperationId},
    pairing::{LocalPairingKey, PendingRevocationTask, RevocationFailure, SignedPairingReceipt},
};
use std::sync::Arc;
use tokio::sync::Mutex;

/// The Pane/view owner invalidates existing incarnations synchronously and
/// captures only self-owned grants belonging to this exact Workspace. Its
/// remove hook closes the exact ordinary registry worker, never a newer pairing.
/// No lifetime effect is sent to server Sessions by either method.
pub trait LocalUseInvalidator: Send + Sync {
    fn invalidate_workspace(
        &self,
        pairing: &LocalPairingKey,
        workspace: WorkspaceId,
    ) -> Result<Vec<GrantedControl>, ClientFailure>;
    /// Reject an obsolete incarnation before invalidation or grant capture.
    fn invalidate_attachment(
        &self,
        pairing: &LocalPairingKey,
        attachment: crate::view::WorkspaceAttachment,
    ) -> Result<Vec<GrantedControl>, ClientFailure>;
    fn invalidate_pairing(&self, pairing: &LocalPairingKey);
    fn remove_pairing<'a>(&'a self, pairing: &'a LocalPairingKey) -> ClientFuture<'a, ()>;
}

/// Pairing implements this driver with restricted authentication, exact receipt
/// verification and native pairing-slot cleanup. There is no ordinary connection
/// or install/Start/update/enrollment capability in the scheduler's dependencies.
pub trait RevocationDriver: Send + Sync {
    /// The driver owns bounded network work and cancellation cleanup. Native
    /// cleanup may outlast a dial deadline and must remain joinable.
    fn resolve<'a>(&'a self, task: &'a JournalRevocation)
    -> ClientFuture<'a, RevocationResolution>;
    /// Returns only after the authenticated server confirms the restricted
    /// acknowledgement, not after transport queue admission.
    fn acknowledge<'a>(&'a self, task: &'a JournalRevocation) -> ClientFuture<'a, ()>;
    fn cleanup<'a>(
        &'a self,
        task: &'a JournalRevocation,
    ) -> ClientFuture<'a, CredentialCleanupDisposition>;
}
#[derive(Clone)]
pub enum RevocationResolution {
    Pending(RevocationFailure),
    Confirmed(Box<SignedPairingReceipt>),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialCleanupDisposition {
    Removed,
    AlreadyAbsent,
    Pending,
}
#[derive(Clone, Debug)]
pub struct DetachReceipt {
    pub local_pairing: LocalPairingKey,
    pub workspace: WorkspaceId,
    pub journal: JournalReceipt,
}
#[derive(Clone, Debug)]
pub struct LocalRemovalReceipt {
    pub local_pairing: LocalPairingKey,
    pub journal: JournalReceipt,
}

#[derive(Clone)]
pub struct PendingTasks {
    journal: Arc<JournalController>,
    invalidator: Arc<dyn LocalUseInvalidator>,
    serial: Arc<Mutex<()>>,
}
impl PendingTasks {
    pub fn new(journal: Arc<JournalController>, invalidator: Arc<dyn LocalUseInvalidator>) -> Self {
        Self {
            journal,
            invalidator,
            serial: Arc::new(Mutex::new(())),
        }
    }
    pub fn journal(&self) -> &Arc<JournalController> {
        &self.journal
    }
    pub async fn detach_workspace(
        &self,
        binding: &PairedServerBinding,
        workspace: WorkspaceId,
    ) -> Result<DetachReceipt, ClientFailure> {
        self.detach(binding, workspace, None).await
    }
    pub async fn detach_attachment(
        &self,
        binding: &PairedServerBinding,
        attachment: crate::view::WorkspaceAttachment,
    ) -> Result<DetachReceipt, ClientFailure> {
        if attachment.pairing() != binding.target().pairing()
            || attachment.client() != binding.target().client()
        {
            return Err(invalid_peer());
        }
        self.detach(binding, attachment.workspace(), Some(attachment))
            .await
    }
    async fn detach(
        &self,
        binding: &PairedServerBinding,
        workspace: WorkspaceId,
        attachment: Option<crate::view::WorkspaceAttachment>,
    ) -> Result<DetachReceipt, ClientFailure> {
        let key = binding.target().local_pairing()?;
        if workspace.server() != binding.target().server() {
            return Err(invalid_peer());
        }
        // Invalidation precedes storage and all notifications. A persistence
        // failure leaves old leases blocked, rather than restoring old authority.
        let grants = if let Some(attachment) = attachment {
            if crate::view::AttachmentSuppression::suppresses(
                &self.journal.snapshot()?,
                &attachment,
            ) {
                return Err(ClientFailure::new(ClientFailureClass::StaleAuthority));
            }
            self.invalidator.invalidate_attachment(&key, attachment)?
        } else {
            self.invalidator.invalidate_workspace(&key, workspace)?
        };
        for grant in &grants {
            if grant.grant().client() != binding.target().client() {
                return Err(invalid_peer());
            }
        }
        let binding = binding.clone();
        let receipt_key = key.clone();
        let (receipt, ()) = self
            .journal
            .update(move |state| {
                if !state.ordinary_use_allowed(&binding) {
                    return Err(ClientFailure::new(ClientFailureClass::AuthorizationDenied));
                }
                let mut grants = grants;
                for retained in state
                    .grants()
                    .iter()
                    .filter(|grant| grant.local_pairing == key && grant.workspace == workspace)
                {
                    if !grants.contains(&retained.control) {
                        grants.push(retained.control);
                    }
                }
                let dependencies = state
                    .operations()
                    .iter()
                    .filter(|record| {
                        record.binding.id.stream().pairing() == binding.target().pairing()
                            && record.binding.id.stream().client() == binding.target().client()
                            && (record.workspace == Some(workspace) || record.workspace.is_none())
                            && record.reserves_grant
                            && matches!(
                                record.state,
                                crate::journal::RetainedOperationState::Pending
                            )
                    })
                    .map(|record| record.binding)
                    .collect();
                let mut changes = vec![ClientJournalChange::BeginDetach(DetachTask {
                    local_pairing: key.clone(),
                    workspace,
                    grants,
                    dependencies,
                    releases: vec![],
                })];
                if let Some(attachment) = attachment {
                    changes.push(ClientJournalChange::SuppressAttachment(
                        crate::journal::JournalAttachment::new(key, attachment)?,
                    ));
                }
                Ok((changes, ()))
            })
            .await?;
        Ok(DetachReceipt {
            local_pairing: receipt_key,
            workspace,
            journal: receipt,
        })
    }
    /// Pairing's deliberate confirmation supplies the canonical purpose-bound
    /// task. This method does not fabricate a restricted stream or enroll.
    pub async fn record_self_revocation(
        &self,
        binding: &PairedServerBinding,
        task: PendingRevocationTask,
    ) -> Result<LocalRemovalReceipt, ClientFailure> {
        let key = binding.target().local_pairing()?;
        if task.parts().local_pairing != key {
            return Err(invalid_peer());
        }
        let record = JournalRevocation {
            task,
            target: SavedTarget::from_target(binding.target()),
            phase: RevocationPhase::AwaitingConfirmation,
            notice_dismissed: false,
            attempts: 0,
        };
        crate::journal::model::validate_revocation(&record, self.journal.verifier())?;
        self.invalidator.invalidate_pairing(&key);
        let owned = self.clone();
        tokio::spawn(async move {
            let receipt = owned
                .journal
                .transact(vec![ClientJournalChange::BeginRevocation(Box::new(record))])
                .await?;
            owned.invalidator.remove_pairing(&key).await?;
            Ok(LocalRemovalReceipt {
                local_pairing: key,
                journal: receipt,
            })
        })
        .await
        .map_err(|_| ClientFailure::new(ClientFailureClass::RecoveryRequired))?
    }
    pub async fn dismiss_revocation(&self, id: OperationId) -> Result<(), ClientFailure> {
        self.journal
            .transact(vec![ClientJournalChange::DismissRevocation(id)])
            .await
            .map(|_| ())
    }

    /// Runs a bounded reconciliation pass. Existing releases are looked up before
    /// resending; a new release is journaled atomically with its detach dependency.
    /// One exact release per frame stays inside the existing management ceiling.
    pub async fn drain_detach(
        &self,
        binding: &PairedServerBinding,
        workspace: WorkspaceId,
        peer: &dyn OperationPeer,
    ) -> Result<bool, ClientFailure> {
        let _serial = self.serial.lock().await;
        let key = binding.target().local_pairing()?;
        let state = self.journal.snapshot()?;
        let Some(task) = state
            .detaches()
            .iter()
            .find(|task| task.local_pairing == key && task.workspace == workspace)
            .cloned()
        else {
            return Ok(true);
        };
        if !state.ordinary_use_allowed(binding) {
            return Err(ClientFailure::new(ClientFailureClass::AuthorizationDenied));
        }
        let tracker = OperationTracker::new(Arc::clone(&self.journal));
        for dependency in &task.dependencies {
            if state.operation(dependency.id).is_some_and(|record| {
                matches!(
                    record.state,
                    crate::journal::RetainedOperationState::Pending
                )
            }) {
                let result = peer.lookup(*dependency).await?;
                tracker.resolve_bound(*dependency, result, peer).await?;
            }
        }
        let state = self.journal.snapshot()?;
        let grants = match crate::journal::model::detach_grants(&state, &task, self.journal.codec())
        {
            Ok(grants) => grants,
            Err(error) if error.class() == ClientFailureClass::StaleAuthority => return Ok(false),
            Err(error) => return Err(error),
        };
        for grant in grants {
            let state = self.journal.snapshot()?;
            let existing = task.releases.iter().find(|operation| state.operation(operation.id).is_some_and(|record| {
                CanonicalOperationRequest::decode(&record.request, self.journal.codec()).is_ok_and(|request| matches!(request.command(), Command::ReleaseControl(release) if release.epoch == grant.epoch() && release.grant == grant.grant()))
            })).copied();
            if let Some(operation) = existing {
                if !crate::journal::model::release_settled(
                    &state,
                    operation,
                    grant,
                    self.journal.codec(),
                ) {
                    if state.operation(operation.id).is_some_and(|record| {
                        matches!(
                            record.state,
                            crate::journal::RetainedOperationState::Pending
                        )
                    }) {
                        let result = peer.lookup(operation).await?;
                        tracker.resolve_bound(operation, result, peer).await?;
                    }
                    if !crate::journal::model::release_settled(
                        &self.journal.snapshot()?,
                        operation,
                        grant,
                        self.journal.codec(),
                    ) {
                        return Ok(false);
                    }
                }
            } else {
                let record = tracker
                    .prepare(
                        binding.clone(),
                        Command::ReleaseControl(ReleaseControlRequest {
                            epoch: grant.epoch(),
                            grant: grant.grant(),
                        }),
                        Some(workspace),
                        true,
                        Some(key.clone()),
                    )
                    .await?;
                let request =
                    CanonicalOperationRequest::decode(&record.request, self.journal.codec())
                        .map_err(|_| invalid_peer())?;
                self.journal.ensure_healthy()?;
                peer.submit(&request).await?;
                return Ok(false);
            }
        }
        self.journal
            .transact(vec![ClientJournalChange::FinishDetach {
                local_pairing: key,
                workspace,
            }])
            .await?;
        Ok(true)
    }

    /// Signed completion is committed before native cleanup. Cleanup failure or
    /// caller interruption resumes that phase, without resubmitting revocation.
    pub async fn retry_revocation(
        &self,
        id: OperationId,
        driver: &dyn RevocationDriver,
    ) -> Result<bool, ClientFailure> {
        let _serial = self.serial.lock().await;
        let state = self.journal.snapshot()?;
        let Some(record) = state
            .revocations()
            .iter()
            .find(|record| record.task.parts().original.operation() == id)
            .cloned()
        else {
            return Ok(true);
        };
        self.journal
            .transact(vec![ClientJournalChange::RevocationAttempt(id)])
            .await?;
        if matches!(record.phase, RevocationPhase::AwaitingConfirmation) {
            match driver.resolve(&record).await? {
                RevocationResolution::Pending(_) => return Ok(false),
                RevocationResolution::Confirmed(receipt) => {
                    self.journal
                        .transact(vec![ClientJournalChange::ConfirmRevocation {
                            operation: id,
                            receipt,
                        }])
                        .await?;
                }
            }
        }
        let record = self
            .journal
            .snapshot()?
            .revocations()
            .iter()
            .find(|record| record.task.parts().original.operation() == id)
            .cloned()
            .ok_or_else(invalid_peer)?;
        self.journal.ensure_healthy()?;
        let restricted = self
            .journal
            .snapshot()?
            .stream(id.stream())
            .cloned()
            .ok_or_else(invalid_peer)?;
        if restricted.confirmed_prefix == 0 {
            driver.acknowledge(&record).await?;
            self.journal
                .transact(vec![ClientJournalChange::ConfirmRevocationAcknowledgement(
                    id,
                )])
                .await?;
        }
        match driver.cleanup(&record).await? {
            CredentialCleanupDisposition::Pending => Ok(false),
            CredentialCleanupDisposition::Removed | CredentialCleanupDisposition::AlreadyAbsent => {
                self.journal
                    .transact(vec![ClientJournalChange::FinishCredentialCleanup(id)])
                    .await?;
                Ok(true)
            }
        }
    }
    /// Pairing's real driver owns one attempt, including receipt verification,
    /// restricted acknowledgement and exact native cleanup. This journal owns
    /// its only clock, backoff, lifetime claim and cancellation drain.
    pub fn start_pairing_scheduler<N, R>(
        &self,
        driver: Arc<crate::pairing::revocation::RevocationDriver<N, R>>,
    ) -> Result<PendingRuntime, ClientFailure>
    where
        N: crate::pairing::credentials::NativeDeviceCredentials + 'static,
        R: crate::pairing::revocation::RestrictedConnector + 'static,
    {
        self.start_pairing_scheduler_inner(driver, None)
    }
    /// Settles enrollment acknowledgements and revocations under the same
    /// journal lifetime claim and retry clock. Historical work never reenrolls.
    pub fn start_pairing_scheduler_with_enrollment<N, R, E>(
        &self,
        driver: Arc<crate::pairing::revocation::RevocationDriver<N, R>>,
        enrollment: Arc<
            crate::pairing::enrollment::EnrollmentDriver<
                crate::pairing::journal::JournalPairingStore<JournalController>,
                N,
                E,
            >,
        >,
        majors: Vec<u32>,
    ) -> Result<PendingRuntime, ClientFailure>
    where
        N: crate::pairing::credentials::NativeDeviceCredentials + 'static,
        R: crate::pairing::revocation::RestrictedConnector + 'static,
        E: crate::pairing::enrollment::EnrollmentConnector + 'static,
    {
        let attempt = Arc::new(move |id| {
            let enrollment = enrollment.clone();
            let majors = majors.clone();
            Box::pin(async move {
                enrollment
                    .acknowledge_once(id, majors)
                    .await
                    .map_err(pairing_failure)
            }) as ClientFuture<'static, ()>
        });
        self.start_pairing_scheduler_inner(driver, Some(attempt))
    }
    fn start_pairing_scheduler_inner<N, R>(
        &self,
        driver: Arc<crate::pairing::revocation::RevocationDriver<N, R>>,
        enrollment: Option<scheduler::EnrollmentAcknowledgementAttempt>,
    ) -> Result<PendingRuntime, ClientFailure>
    where
        N: crate::pairing::credentials::NativeDeviceCredentials + 'static,
        R: crate::pairing::revocation::RestrictedConnector + 'static,
    {
        self.journal.ensure_healthy()?;
        let tasks = self.clone();
        scheduler::start_with_enrollment(
            self.clone(),
            move |id| {
                let tasks = tasks.clone();
                let driver = Arc::clone(&driver);
                Box::pin(async move {
                    let _serial = tasks.serial.lock().await;
                    if !tasks
                        .journal
                        .snapshot()?
                        .revocations()
                        .iter()
                        .any(|record| record.task.parts().original.operation() == id)
                    {
                        return Ok(true);
                    }
                    tasks
                        .journal
                        .transact(vec![ClientJournalChange::RevocationAttempt(id)])
                        .await?;
                    let store = crate::pairing::journal::JournalPairingStore::new(Arc::clone(
                        &tasks.journal,
                    ));
                    driver.run_once(&store, id).await.map_err(pairing_failure)?;
                    Ok(true)
                })
            },
            enrollment,
        )
    }
    pub fn start_scheduler(
        &self,
        driver: Arc<dyn RevocationDriver>,
    ) -> Result<PendingRuntime, ClientFailure> {
        self.journal.ensure_healthy()?;
        let tasks = self.clone();
        scheduler::start(self.clone(), move |id| {
            let tasks = tasks.clone();
            let driver = Arc::clone(&driver);
            Box::pin(async move { tasks.retry_revocation(id, driver.as_ref()).await })
        })
    }
}

fn pairing_failure(failure: spaceterm_core::pairing::PairingFailure) -> ClientFailure {
    use spaceterm_core::pairing::PairingFailure;
    ClientFailure::new(match failure {
        PairingFailure::Offline
        | PairingFailure::TimedOut
        | PairingFailure::RevocationUnconfirmed => ClientFailureClass::Unavailable,
        PairingFailure::AuthorizationRequired
        | PairingFailure::AuthorizationDenied
        | PairingFailure::AuthorizationUnavailable
        | PairingFailure::PairingRevoked
        | PairingFailure::ClientNotBound
        | PairingFailure::CredentialUnavailable => ClientFailureClass::AuthorizationDenied,
        PairingFailure::IdentityMismatch | PairingFailure::FingerprintMismatch => {
            ClientFailureClass::PinMismatch
        }
        PairingFailure::Incompatible => ClientFailureClass::Incompatible,
        PairingFailure::StaleAssessment | PairingFailure::RouteChanged => {
            ClientFailureClass::StaleAuthority
        }
        PairingFailure::LimitExceeded => ClientFailureClass::ResourceLimit,
        PairingFailure::StorageUnavailable => ClientFailureClass::StorageUnavailable,
        PairingFailure::UnsafeStorage => ClientFailureClass::UnsafeStorage,
        PairingFailure::UnknownSchema => ClientFailureClass::UnknownSchema,
        PairingFailure::InvalidDurableState => ClientFailureClass::InvalidStorage,
        PairingFailure::RecoveryRequired => ClientFailureClass::RecoveryRequired,
        PairingFailure::Cancelled => ClientFailureClass::Cancelled,
        _ => ClientFailureClass::InvalidPeer,
    })
}
