use super::*;
use spaceterm_client::{
    api::PairedServerBinding, failure::ClientFailure, operations::OperationPeer,
    ports::ClientFuture,
};
use spaceterm_core::{
    identity::{OperationStreamId, WorkspaceId},
    operation_ledger_contracts::CanonicalOperationRequest,
    operations::{Command, OperationBinding},
};
use std::sync::{Arc, atomic::Ordering};
use tokio::sync::Notify;

struct SubmissionGate {
    peer: Peer,
    entered: Notify,
    resume: Notify,
}
impl OperationPeer for SubmissionGate {
    fn validate_submission(
        &self,
        binding: &PairedServerBinding,
        command: &Command,
        workspace: Option<WorkspaceId>,
    ) -> Result<(), ClientFailure> {
        self.peer.validate_submission(binding, command, workspace)
    }
    fn submit<'a>(&'a self, request: &'a CanonicalOperationRequest) -> ClientFuture<'a, ()> {
        Box::pin(async move {
            let result = self.peer.submit(request).await;
            self.entered.notify_one();
            self.resume.notified().await;
            result
        })
    }
    fn lookup(&self, binding: OperationBinding) -> ClientFuture<'_, OperationLookup> {
        self.peer.lookup(binding)
    }
    fn acknowledge(&self, stream: OperationStreamId, through: u64) -> ClientFuture<'_, ()> {
        self.peer.acknowledge(stream, through)
    }
}

async fn late_send_preserves_terminal_status(fail_send: bool, lookup: OperationLookup) {
    let fixture = Fixture::new().await;
    let tracker = OperationTracker::new(fixture.journal.clone());
    let gate = Arc::new(SubmissionGate {
        peer: Peer::default(),
        entered: Notify::new(),
        resume: Notify::new(),
    });
    gate.peer.fail_send.store(fail_send, Ordering::SeqCst);
    let submitter = tracker.clone();
    let binding = fixture.binding.clone();
    let peer = gate.clone();
    let caller = tokio::spawn(async move {
        submitter
            .submit(&binding, rename(1), Some(workspace()), peer.as_ref())
            .await
    });
    gate.entered.notified().await;
    let operation = gate.peer.sent.lock().unwrap()[0].binding();
    let ticket = tracker.ticket(operation.id).unwrap();
    let expected = match &lookup {
        OperationLookup::Completed {
            result,
            publication_revision,
        } => OperationStatus::Completed {
            outcome: result.clone(),
            publication_revision: *publication_revision,
        },
        OperationLookup::AcknowledgedRetired => OperationStatus::Retired,
        _ => panic!("terminal lookup fixture required"),
    };
    tracker
        .resolve_bound(operation, lookup, &fixture.peer)
        .await
        .unwrap();
    assert_eq!(*ticket.status().borrow(), expected);

    gate.resume.notify_one();
    let returned = caller.await.unwrap().unwrap();
    assert_eq!(returned.binding(), operation);
    assert_eq!(*returned.status().borrow(), expected);
    assert_eq!(*ticket.status().borrow(), *returned.status().borrow());
    assert_eq!(
        *tracker.ticket(operation.id).unwrap().status().borrow(),
        *returned.status().borrow()
    );
}

#[tokio::test]
async fn late_successful_send_cannot_replace_durable_completed_ticket() {
    late_send_preserves_terminal_status(false, completed(7)).await;
}

#[tokio::test]
async fn late_failed_send_cannot_replace_durable_completed_ticket() {
    late_send_preserves_terminal_status(true, completed(7)).await;
}

#[tokio::test]
async fn late_successful_send_cannot_replace_retired_ticket() {
    late_send_preserves_terminal_status(false, OperationLookup::AcknowledgedRetired).await;
}

#[tokio::test]
async fn late_failed_send_cannot_replace_retired_ticket() {
    late_send_preserves_terminal_status(true, OperationLookup::AcknowledgedRetired).await;
}
