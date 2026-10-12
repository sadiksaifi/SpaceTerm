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

struct Shared {
    storage: Arc<dyn ClientJournal>,
    codec: Arc<dyn LedgerCodec>,
    verifier: Arc<dyn ReceiptVerifier>,
    limits: JournalLimits,
    serial: AsyncMutex<()>,
    state: Mutex<RecoveredClientState>,
    closed: AtomicBool,
    health: watch::Sender<JournalHealth>,
    revisions: watch::Sender<ClientJournalRevision>,
    scheduler: Arc<AtomicBool>,
}
struct CommitGuard {
    shared: Arc<Shared>,
    resolved: bool,
}
impl Drop for CommitGuard {
    fn drop(&mut self) {
        if !self.resolved {
            self.shared.closed.store(true, Ordering::Release);
            self.shared
                .health
                .send_replace(JournalHealth::RecoveryRequired);
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
        let (health, _) = watch::channel(JournalHealth::Ready);
        let (revisions, _) = watch::channel(state.revision());
        Ok(Self {
            shared: Arc::new(Shared {
                storage,
                codec,
                verifier,
                limits,
                serial: AsyncMutex::new(()),
                state: Mutex::new(state),
                closed: AtomicBool::new(false),
                health,
                revisions,
                scheduler: Arc::new(AtomicBool::new(false)),
            }),
        })
    }
    pub fn ensure_healthy(&self) -> Result<(), ClientFailure> {
        if self.shared.closed.load(Ordering::Acquire) {
            Err(JournalFailure::RecoveryRequired.into())
        } else {
            Ok(())
        }
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
            if shared.closed.load(Ordering::Acquire) {
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
                    shared.closed.store(true, Ordering::Release);
                    shared.health.send_replace(JournalHealth::RecoveryRequired);
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
            self.shared.closed.store(true, Ordering::Release);
            self.shared
                .health
                .send_replace(JournalHealth::RecoveryRequired);
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
        Box::pin(async move { Ok(self.snapshot()?.ordinary_use_allowed(binding)) })
    }
}
