//! One bounded, dedicated blocking worker owns the store for its whole lifetime.
//! Cancellation of a caller cannot cancel a started physical commit.

use crate::contracts::{ServerFailure, ServerFailureClass};
use crate::failure::{failure, store_failure};
use spaceterm_core::storage::*;
use std::sync::Arc;
use std::thread::JoinHandle;
use tokio::sync::{mpsc, oneshot};

/// Compose the host SQL projection and domain validators with pairing's exact-byte
/// receipt reader. The pin and home are comparison facts supplied by native startup;
/// they grant no filesystem authority and select no credentials.
pub fn production_services(
    identity: spaceterm_host::organization_store::StoreIdentity,
    home: spaceterm_core::organization::ServerDirectory,
) -> Result<spaceterm_host::organization_store::StoreServices, StoreFailure> {
    if home.server() != identity.server
        || identity.account.account_uid == 0
        || identity.account.account_uid == u32::MAX
    {
        return Err(StoreFailure::IdentityMismatch);
    }
    let reader = crate::pairing::store_projection::SignedReceiptReader::new(
        spaceterm_protocol::authentication::ServerPin {
            identity: identity.server.bytes(),
            public_key: identity.server_public_key,
        },
    )?;
    Ok(spaceterm_host::organization_store::StoreServices::production(home, Arc::new(reader)))
}

enum StoreRequest {
    Recover(oneshot::Sender<Result<RecoveredState, StoreFailure>>),
    Commit(Arc<DurableTransaction>, oneshot::Sender<CommitDisposition>),
}

pub struct StoreWorker {
    sender: Option<mpsc::Sender<StoreRequest>>,
    join: Option<JoinHandle<()>>,
}
impl StoreWorker {
    pub fn new(mut store: Box<dyn OrganizationStore>) -> Result<Self, ServerFailure> {
        let (sender, mut receiver) = mpsc::channel(1);
        let join = std::thread::Builder::new().name("spaceterm-store".into()).spawn(move || {
            let mut poisoned = false;
            while let Some(request) = receiver.blocking_recv() {
                match request {
                    StoreRequest::Recover(reply) => {
                        let _ = reply.send(if poisoned { Err(StoreFailure::RecoveryRequired) } else { store.recover() });
                    }
                    StoreRequest::Commit(transaction, reply) => {
                        let disposition = if poisoned { CommitDisposition::NotApplied(StoreFailure::RecoveryRequired) } else { store.commit(&transaction) };
                        // A malformed receipt is just as unsafe as an uncertain commit.
                        poisoned |= matches!(disposition, CommitDisposition::Indeterminate)
                            || matches!(disposition, CommitDisposition::Durable(receipt) if !transaction.accepts_receipt(receipt));
                        let _ = reply.send(disposition);
                    }
                }
            }
        }).map_err(|_| failure(ServerFailureClass::StorageUnavailable))?;
        Ok(Self {
            sender: Some(sender),
            join: Some(join),
        })
    }
    pub async fn recover(&self) -> Result<RecoveredState, ServerFailure> {
        let (reply, result) = oneshot::channel();
        self.sender
            .as_ref()
            .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?
            .send(StoreRequest::Recover(reply))
            .await
            .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?;
        result
            .await
            .map_err(|_| failure(ServerFailureClass::RecoveryRequired))?
            .map_err(store_failure)
    }
    pub fn commit(
        &self,
        transaction: Arc<DurableTransaction>,
    ) -> Result<oneshot::Receiver<CommitDisposition>, ServerFailure> {
        let (reply, result) = oneshot::channel();
        self.sender
            .as_ref()
            .ok_or_else(|| failure(ServerFailureClass::RecoveryRequired))?
            .try_send(StoreRequest::Commit(transaction, reply))
            .map_err(|_| failure(ServerFailureClass::Capacity))?;
        Ok(result)
    }
    pub async fn close(mut self) -> Result<(), ServerFailure> {
        self.sender.take();
        if let Some(join) = self.join.take() {
            tokio::task::spawn_blocking(move || join.join())
                .await
                .map_err(|_| failure(ServerFailureClass::CleanupIncomplete))?
                .map_err(|_| failure(ServerFailureClass::CleanupIncomplete))?;
        }
        Ok(())
    }
}
impl Drop for StoreWorker {
    fn drop(&mut self) {
        // Closing ingress drains any in-flight commit before the store is dropped.
        // Explicit shutdown joins the worker; unexpected drop claims no clean stop.
        self.sender.take();
    }
}
