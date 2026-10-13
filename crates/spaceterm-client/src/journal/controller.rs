use super::*;
use crate::ports::ConnectionJournal;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::{Mutex as AsyncMutex, watch};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JournalHealth {
    Ready,
    RecoveryRequired,
}

pub(super) struct Shared {
    storage: Arc<dyn ClientJournal>,
    codec: Arc<dyn LedgerCodec>,
    verifier: Arc<dyn ReceiptVerifier>,
    limits: JournalLimits,
    serial: AsyncMutex<()>,
    state: Mutex<RecoveredClientState>,
    closed: Arc<JournalAdmission>,
    health: watch::Sender<JournalHealth>,
    revisions: watch::Sender<ClientJournalRevision>,
    scheduler: Arc<AtomicBool>,
    panes: Mutex<Option<crate::pane_channel::PaneChannels>>,
}
impl Shared {
    fn close(&self) {
        self.closed.close();
    }
    pub(super) fn revoke(&self) {
        if let Ok(state) = self.state.lock() {
            for binding in state.bindings() {
                if let Ok(key) = binding
                    .target
                    .target()
                    .and_then(|target| target.local_pairing())
                {
                    self.storage.authority_fence().seal(&key);
                }
                if let Ok(panes) = self.panes.lock()
                    && let Some(panes) = panes.as_ref()
                {
                    let _ = panes.invalidate(
                        binding.target.server,
                        crate::ports::ConnectionInvalidation::BindingRemoved,
                    );
                }
            }
        }
        self.health.send_replace(JournalHealth::RecoveryRequired);
    }
}
/// Pane eligibility observes the same monotonic native latch and pairing seal.
/// A weak owner avoids a journal/channels ownership cycle. Contended journal
/// state denies this effect rather than blocking a dispatch or native callback.
pub(crate) struct PaneJournalAdmission(std::sync::Weak<Shared>);
impl PaneJournalAdmission {
    pub(crate) fn allows(&self, info: crate::api::ConnectedInfo) -> bool {
        let Some(shared) = self.0.upgrade() else {
            return false;
        };
        if shared.closed.is_closed() {
            return false;
        }
        let Ok(state) = shared.state.try_lock() else {
            return false;
        };
        let controller = JournalController {
            shared: Arc::clone(&shared),
        };
        state.bindings().iter().any(|saved| {
            saved.binding().is_ok_and(|binding| {
                binding.target().server() == info.server
                    && binding.target().pairing() == info.pairing
                    && binding.target().client() == info.client
                    && controller
                        .ordinary_use_allowed_in(&state, &binding)
                        .unwrap_or(false)
            })
        })
    }
}
struct CommitGuard {
    shared: Arc<Shared>,
    resolved: bool,
}
impl Drop for CommitGuard {
    fn drop(&mut self) {
        if !self.resolved {
            self.shared.close();
        }
    }
}
#[derive(Clone)]
pub struct JournalController {
    shared: Arc<Shared>,
}
impl JournalController {
    pub async fn recover(
        storage: Arc<dyn ClientJournal>,
        codec: Arc<dyn LedgerCodec>,
        verifier: Arc<dyn ReceiptVerifier>,
        limits: JournalLimits,
    ) -> Result<Self, ClientFailure> {
        let limits = limits.validate()?;
        let state = storage.recover().await?;
        state.validate(codec.as_ref(), verifier.as_ref(), limits)?;
        let scheduler = storage.scheduler_claim();
        let closed = storage.admission_latch();
        let (health, _) = watch::channel(JournalHealth::Ready);
        let (revisions, _) = watch::channel(state.revision());
        let shared = Arc::new(Shared {
            storage,
            codec,
            verifier,
            limits,
            serial: AsyncMutex::new(()),
            state: Mutex::new(state),
            closed,
            health,
            revisions,
            scheduler,
            panes: Mutex::new(None),
        });
        shared.closed.register(&shared)?;
        Ok(Self { shared })
    }
    /// Journal uncertainty revokes live dispatch before health notification.
    pub fn bind_pane_channels(
        &self,
        channels: crate::pane_channel::PaneChannels,
    ) -> Result<(), ClientFailure> {
        let mut owner = self.shared.panes.lock().map_err(|_| invalid())?;
        self.ensure_healthy()?;
        if owner.is_some() {
            return Err(crate::failure::resource_limit());
        }
        channels.bind_journal_admission(PaneJournalAdmission(Arc::downgrade(&self.shared)))?;
        *owner = Some(channels);
        Ok(())
    }
    pub fn ensure_healthy(&self) -> Result<(), ClientFailure> {
        if self.shared.closed.is_closed() {
            Err(JournalFailure::RecoveryRequired.into())
        } else {
            Ok(())
        }
    }
    /// Connection and operation admission share pairing's synchronous seal.
    pub fn ordinary_use_allowed(
        &self,
        binding: &PairedServerBinding,
    ) -> Result<bool, ClientFailure> {
        self.ordinary_use_allowed_in(&self.snapshot()?, binding)
    }
    pub(crate) fn ordinary_use_allowed_in(
        &self,
        state: &RecoveredClientState,
        binding: &PairedServerBinding,
    ) -> Result<bool, ClientFailure> {
        self.ensure_healthy()?;
        if !state.ordinary_use_allowed(binding) {
            return Ok(false);
        }
        // Legacy records remain readable. They have no native pairing scope;
        // native credential delivery still supplies no ordinary signing access.
        if let Some(slot) = state.pairing().slot(binding.target().credential()) {
            let snapshot = state.pairing_snapshot().map_err(|_| invalid())?;
            let saved = snapshot
                .active(&binding.target().local_pairing()?)
                .ok_or_else(invalid)?;
            self.shared
                .storage
                .authority_fence()
                .observe(&snapshot)
                .map_err(|_| invalid())?;
            return Ok(self
                .shared
                .storage
                .authority_fence()
                .issue(saved.key(), &saved.credential().scope)
                .is_ok_and(|authority| {
                    authority
                        .lease()
                        .validate(&saved.credential().scope)
                        .is_ok()
                })
                && slot.public_key.is_some());
        }
        Ok(true)
    }
    pub fn health(&self) -> watch::Receiver<JournalHealth> {
        self.shared.health.subscribe()
    }
    pub fn snapshot(&self) -> Result<RecoveredClientState, ClientFailure> {
        self.ensure_healthy()?;
        self.shared
            .state
            .lock()
            .map(|state| state.clone())
            .map_err(|_| invalid())
    }
    pub fn codec(&self) -> &dyn LedgerCodec {
        self.shared.codec.as_ref()
    }
    pub fn limits(&self) -> JournalLimits {
        self.shared.limits
    }
    pub(crate) fn verifier(&self) -> &dyn ReceiptVerifier {
        self.shared.verifier.as_ref()
    }
    pub fn revisions(&self) -> watch::Receiver<ClientJournalRevision> {
        self.shared.revisions.subscribe()
    }
    pub(crate) fn claim_scheduler(&self) -> Result<Arc<AtomicBool>, ClientFailure> {
        self.shared
            .scheduler
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| crate::failure::resource_limit())?;
        Ok(Arc::clone(&self.shared.scheduler))
    }
    pub async fn transact(
        &self,
        changes: Vec<ClientJournalChange>,
    ) -> Result<JournalReceipt, ClientFailure> {
        self.update(move |_| Ok((changes, ())))
            .await
            .map(|(receipt, ())| receipt)
    }

    /// Allocation and validation run inside the same serial owner as persistence.
    /// Once accepted, dropping a caller never cancels or abandons the journal write.
    pub async fn update<T: Send + 'static>(
        &self,
        prepare: impl FnOnce(
            &RecoveredClientState,
        ) -> Result<(Vec<ClientJournalChange>, T), ClientFailure>
        + Send
        + 'static,
    ) -> Result<(JournalReceipt, T), ClientFailure> {
        self.ensure_healthy()?;
        let shared = Arc::clone(&self.shared);
        tokio::spawn(async move {
            let _serial = shared.serial.lock().await;
            if shared.closed.is_closed() {
                return Err(JournalFailure::RecoveryRequired.into());
            }
            let state = shared.state.lock().map_err(|_| invalid())?.clone();
            let (changes, value) = prepare(&state)?;
            let tx = ClientJournalTransaction {
                expected_revision: state.revision(),
                changes,
            };
            let candidate = state.apply(
                &tx,
                shared.codec.as_ref(),
                shared.verifier.as_ref(),
                shared.limits,
            )?;
            for change in &tx.changes {
                if let ClientJournalChange::BeginRevocation(record) = change {
                    shared
                        .storage
                        .authority_fence()
                        .seal(&record.task.parts().local_pairing);
                    if let Ok(panes) = shared.panes.lock()
                        && let Some(panes) = panes.as_ref()
                    {
                        panes.invalidate(
                            record.target.server,
                            crate::ports::ConnectionInvalidation::BindingRemoved,
                        )?;
                    }
                }
            }
            // The latch belongs to the admitted write, not its possibly cancelled
            // waiter. Panic or runtime cancellation after native admission leaves
            // the owner closed even when nobody still awaits this JoinHandle.
            let mut commit = CommitGuard {
                shared: Arc::clone(&shared),
                resolved: false,
            };
            let result = shared.storage.commit(&tx).await;
            match result {
                Ok(JournalCommitDisposition::Durable(receipt))
                    if receipt.revision() == candidate.revision() =>
                {
                    let revision = candidate.revision();
                    *shared.state.lock().map_err(|_| invalid())? = candidate;
                    shared.revisions.send_replace(revision);
                    commit.resolved = true;
                    Ok((receipt, value))
                }
                other => {
                    // Even known NotApplied closes this owner's admission. A
                    // failed preparation must never allocate another identity in
                    // this live owner. Fresh validated recovery owns resumption.
                    shared.close();
                    commit.resolved = true;
                    Err(match other {
                        Ok(JournalCommitDisposition::NotApplied(failure)) => failure.into(),
                        _ => JournalFailure::RecoveryRequired.into(),
                    })
                }
            }
        })
        .await
        .map_err(|_| {
            self.shared.close();
            ClientFailure::from(JournalFailure::RecoveryRequired)
        })?
    }
}
impl ConnectionJournal for JournalController {
    fn recover_bindings(&self) -> ClientFuture<'_, Vec<PairedServerBinding>> {
        Box::pin(async move {
            self.snapshot()?
                .bindings
                .iter()
                .map(SavedBinding::binding)
                .collect()
        })
    }
    fn ordinary_use_allowed<'a>(
        &'a self,
        binding: &'a PairedServerBinding,
    ) -> ClientFuture<'a, bool> {
        Box::pin(async move { self.ordinary_use_allowed(binding) })
    }
}

impl ClientJournal for JournalController {
    fn admission_latch(&self) -> Arc<JournalAdmission> {
        Arc::clone(&self.shared.closed)
    }
    fn scheduler_claim(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.shared.scheduler)
    }
    fn authority_fence(&self) -> &PairingAuthorityFence {
        self.shared.storage.authority_fence()
    }
    fn recover(&self) -> ClientFuture<'_, RecoveredClientState> {
        Box::pin(async move { self.snapshot() })
    }
    fn commit<'a>(
        &'a self,
        transaction: &'a ClientJournalTransaction,
    ) -> ClientFuture<'a, JournalCommitDisposition> {
        let transaction = transaction.clone();
        Box::pin(async move {
            if self.snapshot()?.revision() != transaction.expected_revision {
                return Ok(JournalCommitDisposition::NotApplied(
                    JournalFailure::Conflict,
                ));
            }
            match self
                .update(move |state| {
                    if state.revision() != transaction.expected_revision {
                        return Err(ClientFailure::new(ClientFailureClass::StaleAuthority));
                    }
                    Ok((transaction.changes, ()))
                })
                .await
            {
                Ok((receipt, ())) => Ok(JournalCommitDisposition::Durable(receipt)),
                Err(error) if error.class() == ClientFailureClass::StaleAuthority => Ok(
                    JournalCommitDisposition::NotApplied(JournalFailure::Conflict),
                ),
                Err(error) => Err(error),
            }
        })
    }
}
