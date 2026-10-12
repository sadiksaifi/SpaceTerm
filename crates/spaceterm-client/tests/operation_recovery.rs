#[path = "operation_recovery/support.rs"]
mod support;

use spaceterm_client::{
    failure::ClientFailureClass,
    journal::{ClientJournalChange, JournalCommitDisposition},
    operations::{OperationStatus, OperationTracker},
};
use spaceterm_core::{
    authority::OrganizationRevision,
    operations::{OperationLookup, Outcome},
};
use support::*;

#[tokio::test]
async fn request_is_durable_before_first_send() {
    let fixture = Fixture::new().await;
    let tracker = OperationTracker::new(fixture.journal.clone());
    let ticket = tracker
        .submit(
            &fixture.binding,
            rename(1),
            Some(workspace()),
            &fixture.peer,
        )
        .await
        .unwrap();
    let state = fixture.storage.state();
    assert_eq!(state.operations()[0].binding.id, ticket.id());
    assert_eq!(
        fixture.peer.sent.lock().unwrap()[0].binding(),
        state.operations()[0].binding
    );
}

#[tokio::test]
async fn failed_request_commit_sends_nothing_and_does_not_reuse_identity() {
    let fixture = Fixture::new().await;
    fixture
        .storage
        .fail_next(JournalCommitDisposition::NotApplied(
            spaceterm_client::journal::JournalFailure::Unavailable,
        ));
    let tracker = OperationTracker::new(fixture.journal.clone());
    assert!(
        tracker
            .submit(
                &fixture.binding,
                rename(1),
                Some(workspace()),
                &fixture.peer
            )
            .await
            .is_err()
    );
    assert!(fixture.peer.sent.lock().unwrap().is_empty());
    assert_eq!(
        tracker
            .submit(
                &fixture.binding,
                rename(2),
                Some(workspace()),
                &fixture.peer
            )
            .await
            .unwrap_err()
            .class(),
        ClientFailureClass::RecoveryRequired
    );
}

#[tokio::test]
async fn outcome_is_durable_before_acknowledgement() {
    let fixture = Fixture::new().await;
    let tracker = OperationTracker::new(fixture.journal.clone());
    let ticket = tracker
        .submit(
            &fixture.binding,
            rename(1),
            Some(workspace()),
            &fixture.peer,
        )
        .await
        .unwrap();
    fixture
        .storage
        .fail_next(JournalCommitDisposition::Indeterminate);
    assert!(
        tracker
            .resolve(
                ticket.id(),
                OperationLookup::Completed {
                    result: Outcome::Applied,
                    publication_revision: OrganizationRevision::new(3)
                },
                &fixture.peer
            )
            .await
            .is_err()
    );
    assert!(fixture.peer.acks.lock().unwrap().is_empty());
    assert!(matches!(
        *ticket.status().borrow(),
        OperationStatus::AwaitingResolution
    ));
}

#[tokio::test]
async fn out_of_order_outcomes_do_not_acknowledge_a_gap() {
    let fixture = Fixture::new().await;
    let tracker = OperationTracker::new(fixture.journal.clone());
    let first = tracker
        .submit(
            &fixture.binding,
            rename(1),
            Some(workspace()),
            &fixture.peer,
        )
        .await
        .unwrap();
    let second = tracker
        .submit(
            &fixture.binding,
            rename(1),
            Some(workspace()),
            &fixture.peer,
        )
        .await
        .unwrap();
    tracker
        .resolve(second.id(), completed(7), &fixture.peer)
        .await
        .unwrap();
    assert!(fixture.peer.acks.lock().unwrap().is_empty());
    tracker
        .resolve(first.id(), completed(6), &fixture.peer)
        .await
        .unwrap();
    assert_eq!(
        fixture.peer.acks.lock().unwrap().as_slice(),
        &[(first.id().stream(), 2)]
    );
}

#[tokio::test]
async fn crash_after_outcome_commit_before_ack_recovers_same_result() {
    let fixture = Fixture::new().await;
    let tracker = OperationTracker::new(fixture.journal.clone());
    let ticket = tracker
        .submit(
            &fixture.binding,
            rename(1),
            Some(workspace()),
            &fixture.peer,
        )
        .await
        .unwrap();
    fixture
        .peer
        .fail_ack
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(
        tracker
            .resolve(ticket.id(), completed(5), &fixture.peer)
            .await
            .is_err()
    );
    let recovered = fixture.reopen().await;
    let tracker = OperationTracker::new(recovered);
    tracker
        .reconcile(&fixture.binding, &fixture.peer)
        .await
        .unwrap();
    assert!(
        matches!(*tracker.ticket(ticket.id()).unwrap().status().borrow(), OperationStatus::Completed { publication_revision, .. } if publication_revision == OrganizationRevision::new(5))
    );
    assert_eq!(fixture.peer.sent.lock().unwrap().len(), 1);
    assert_eq!(fixture.peer.acks.lock().unwrap().last().unwrap().1, 1);
}

#[tokio::test]
async fn different_contents_for_same_identity_is_rejected() {
    let fixture = Fixture::new().await;
    let tracker = OperationTracker::new(fixture.journal.clone());
    let ticket = tracker
        .submit(
            &fixture.binding,
            rename(1),
            Some(workspace()),
            &fixture.peer,
        )
        .await
        .unwrap();
    tracker
        .resolve(ticket.id(), completed(5), &fixture.peer)
        .await
        .unwrap();
    let error = tracker
        .resolve(ticket.id(), completed(6), &fixture.peer)
        .await
        .unwrap_err();
    assert_eq!(error.class(), ClientFailureClass::InvalidPeer);
}

#[tokio::test]
async fn server_confirmation_does_not_compact_unconsumed_ui_result() {
    let fixture = Fixture::new().await;
    let tracker = OperationTracker::new(fixture.journal.clone());
    let ticket = tracker
        .submit(
            &fixture.binding,
            rename(1),
            Some(workspace()),
            &fixture.peer,
        )
        .await
        .unwrap();
    tracker
        .resolve(ticket.id(), completed(5), &fixture.peer)
        .await
        .unwrap();
    tracker
        .confirm_acknowledgement(ticket.id().stream(), 1)
        .await
        .unwrap();
    assert_eq!(fixture.journal.snapshot().unwrap().operations().len(), 1);
    fixture
        .journal
        .transact(vec![ClientJournalChange::ConsumeResult(ticket.id())])
        .await
        .unwrap();
    assert!(fixture.journal.snapshot().unwrap().operations().is_empty());
}

fn completed(revision: u64) -> OperationLookup {
    OperationLookup::Completed {
        result: Outcome::Applied,
        publication_revision: OrganizationRevision::new(revision),
    }
}

#[tokio::test]
async fn lost_create_ack_resolves_original_identity_without_duplicate_creation() {
    let fixture = Fixture::new().await;
    fixture
        .peer
        .fail_send
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let tracker = OperationTracker::new(fixture.journal.clone());
    let ticket = tracker
        .submit(
            &fixture.binding,
            spaceterm_core::operations::Command::CreateWorkspace {
                server: server(),
                source: spaceterm_core::operations::WorkspaceCreationSource::ServerHome {
                    server: server(),
                },
                name: spaceterm_core::organization::NameDraft::Automatic,
            },
            None,
            &fixture.peer,
        )
        .await
        .unwrap();
    fixture.peer.results.lock().unwrap().push((
        ticket.id(),
        OperationLookup::Completed {
            result: Outcome::CreatedWithControl(
                spaceterm_core::operations::CreatedOrganization::new(
                    spaceterm_core::operations::CreatedEntities::Workspace {
                        workspace: workspace(),
                        tab: spaceterm_core::identity::TabId::new(server(), 1).unwrap(),
                        pane: spaceterm_core::identity::PaneId::new(server(), 1).unwrap(),
                    },
                    spaceterm_core::authority::GrantedControl::new(
                        spaceterm_core::authority::ServerEpoch::new(1),
                        spaceterm_core::authority::ControlGrant::new(
                            spaceterm_core::identity::PaneId::new(server(), 1).unwrap(),
                            fixture.binding.target().client(),
                            spaceterm_core::authority::ControlGeneration::new(1),
                        )
                        .unwrap(),
                    )
                    .unwrap(),
                )
                .unwrap(),
            ),
            publication_revision: OrganizationRevision::new(9),
        },
    ));
    let reopened = fixture.reopen().await;
    let tracker = OperationTracker::new(reopened);
    tracker
        .reconcile(&fixture.binding, &fixture.peer)
        .await
        .unwrap();
    assert_eq!(fixture.peer.sent.lock().unwrap().len(), 1);
    assert_eq!(fixture.peer.lookups.lock().unwrap()[0], ticket.binding());
    assert!(matches!(
        *tracker.ticket(ticket.id()).unwrap().status().borrow(),
        OperationStatus::Completed { .. }
    ));
}

#[tokio::test]
async fn never_admitted_retry_preserves_exact_request_and_field_version() {
    let fixture = Fixture::new().await;
    let tracker = OperationTracker::new(fixture.journal.clone());
    let ticket = tracker
        .submit(
            &fixture.binding,
            rename(17),
            Some(workspace()),
            &fixture.peer,
        )
        .await
        .unwrap();
    tracker
        .resolve(ticket.id(), OperationLookup::NeverAdmitted, &fixture.peer)
        .await
        .unwrap();
    let sent = fixture.peer.sent.lock().unwrap();
    assert_eq!(sent[0].binding(), sent[1].binding());
    assert_eq!(sent[1].command(), &rename(17));
}

#[tokio::test]
async fn retired_or_expired_identity_never_resubmits() {
    let fixture = Fixture::new().await;
    let tracker = OperationTracker::new(fixture.journal.clone());
    let ticket = tracker
        .submit(
            &fixture.binding,
            rename(1),
            Some(workspace()),
            &fixture.peer,
        )
        .await
        .unwrap();
    tracker
        .resolve(
            ticket.id(),
            OperationLookup::AcknowledgedRetired,
            &fixture.peer,
        )
        .await
        .unwrap();
    assert!(
        tracker
            .resolve(ticket.id(), OperationLookup::NeverAdmitted, &fixture.peer)
            .await
            .is_err()
    );
    assert_eq!(fixture.peer.sent.lock().unwrap().len(), 1);
    assert!(matches!(
        *ticket.status().borrow(),
        OperationStatus::Retired
    ));
}

#[tokio::test]
async fn conflict_preserves_draft_and_current_field_value() {
    use spaceterm_core::{
        authority::FieldVersion,
        operations::{Command, FieldConflict},
        organization::{NameDraft, NameText, WorkspaceName},
    };
    let fixture = Fixture::new().await;
    let tracker = OperationTracker::new(fixture.journal.clone());
    let draft = Command::RenameWorkspace {
        workspace: workspace(),
        expected_version: FieldVersion::new(1),
        draft: NameDraft::Custom(NameText::new("draft".into()).unwrap()),
    };
    let ticket = tracker
        .submit(
            &fixture.binding,
            draft.clone(),
            Some(workspace()),
            &fixture.peer,
        )
        .await
        .unwrap();
    let conflict = FieldConflict::WorkspaceName {
        workspace: workspace(),
        current_value: WorkspaceName {
            mode: spaceterm_core::organization::NameMode::Custom,
            display: NameText::new("current".into()).unwrap(),
        },
        current_version: FieldVersion::new(2),
    };
    tracker
        .resolve(
            ticket.id(),
            OperationLookup::Completed {
                result: Outcome::Conflict(conflict.clone()),
                publication_revision: OrganizationRevision::new(3),
            },
            &fixture.peer,
        )
        .await
        .unwrap();
    tracker
        .confirm_acknowledgement(ticket.id().stream(), 1)
        .await
        .unwrap();
    let reopened = OperationTracker::new(fixture.reopen().await);
    assert_eq!(
        reopened.conflict(ticket.id()).unwrap(),
        Some((draft, conflict))
    );
}

#[tokio::test]
async fn acknowledgement_ahead_of_durable_prefix_is_rejected() {
    let fixture = Fixture::new().await;
    let tracker = OperationTracker::new(fixture.journal.clone());
    let ticket = tracker
        .submit(
            &fixture.binding,
            rename(1),
            Some(workspace()),
            &fixture.peer,
        )
        .await
        .unwrap();
    assert_eq!(
        tracker
            .confirm_acknowledgement(ticket.id().stream(), 1)
            .await
            .unwrap_err()
            .class(),
        ClientFailureClass::InvalidPeer
    );
    assert_eq!(
        fixture
            .journal
            .snapshot()
            .unwrap()
            .stream(ticket.id().stream())
            .unwrap()
            .confirmed_prefix,
        0
    );
}

#[tokio::test]
async fn indeterminate_request_commit_recovers_original_identity_before_resend() {
    let fixture = Fixture::new().await;
    fixture.storage.apply_then_indeterminate();
    let tracker = OperationTracker::new(fixture.journal.clone());
    assert!(
        tracker
            .submit(
                &fixture.binding,
                rename(1),
                Some(workspace()),
                &fixture.peer
            )
            .await
            .is_err()
    );
    assert!(fixture.peer.sent.lock().unwrap().is_empty());
    let original = fixture.storage.state().operations()[0].binding;
    fixture
        .peer
        .results
        .lock()
        .unwrap()
        .push((original.id, OperationLookup::NeverAdmitted));
    let tracker = OperationTracker::new(fixture.reopen().await);
    tracker
        .reconcile(&fixture.binding, &fixture.peer)
        .await
        .unwrap();
    assert_eq!(fixture.peer.sent.lock().unwrap()[0].binding(), original);
    assert_eq!(fixture.peer.lookups.lock().unwrap()[0], original);
}

struct CommitGate {
    panic_after_commit: bool,
    storage: std::sync::Arc<MemoryJournal>,
    entered: tokio::sync::Notify,
    resume: tokio::sync::Notify,
}
impl spaceterm_client::journal::ClientJournal for CommitGate {
    fn recover(
        &self,
    ) -> spaceterm_client::ports::ClientFuture<'_, spaceterm_client::journal::RecoveredClientState>
    {
        self.storage.recover()
    }
    fn commit<'a>(
        &'a self,
        tx: &'a spaceterm_client::journal::ClientJournalTransaction,
    ) -> spaceterm_client::ports::ClientFuture<'a, JournalCommitDisposition> {
        Box::pin(async move {
            self.entered.notify_one();
            self.resume.notified().await;
            let result = self.storage.commit(tx).await;
            assert!(
                !self.panic_after_commit,
                "test journal provider panic after a possibly applied write"
            );
            result
        })
    }
}
#[tokio::test]
async fn caller_cancellation_does_not_abandon_admitted_journal_write() {
    let fixture = Fixture::new().await;
    let gate = std::sync::Arc::new(CommitGate {
        panic_after_commit: false,
        storage: fixture.storage.clone(),
        entered: tokio::sync::Notify::new(),
        resume: tokio::sync::Notify::new(),
    });
    let journal = std::sync::Arc::new(
        spaceterm_client::journal::JournalController::recover(
            gate.clone(),
            fixture.codec.clone(),
            std::sync::Arc::new(Verifier),
            spaceterm_client::journal::JournalLimits::default(),
        )
        .await
        .unwrap(),
    );
    let tracker = OperationTracker::new(journal.clone());
    let binding = fixture.binding.clone();
    let caller = tokio::spawn(async move {
        tracker
            .submit(&binding, rename(1), Some(workspace()), &Peer::default())
            .await
    });
    gate.entered.notified().await;
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    gate.resume.notify_one();
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    assert_eq!(journal.snapshot().unwrap().operations().len(), 1);
    assert_eq!(fixture.storage.state().operations().len(), 1);
}

#[tokio::test]
async fn retry_preserves_old_startup_fence() {
    use spaceterm_core::{
        authority::*,
        control::{StartupIntent, StartupSource},
        identity::PaneId,
        operations::{Command, OperationId, OperationSequence},
    };
    let fixture = Fixture::new().await;
    let stream = fixture.journal.snapshot().unwrap().bindings()[0]
        .streams()
        .parts()
        .ordinary;
    let pane = PaneId::new(server(), 1).unwrap();
    let fence = EffectFence::new(
        SessionKey {
            epoch: ServerEpoch::new(4),
            pane,
            generation: SessionGeneration::new(7),
        },
        ControlGrant::new(
            pane,
            fixture.binding.target().client(),
            ControlGeneration::new(8),
        )
        .unwrap(),
        ConnectionBinding::new(
            fixture.binding.target().client(),
            ConnectionGeneration::new(9),
        ),
        ContextGeneration::new(10),
        None,
    )
    .unwrap();
    let command = Command::BeginStartup(
        StartupIntent::new(
            OperationId::new(stream, OperationSequence::new(1).unwrap()),
            fence,
            StartupSource::ExplicitSelection,
        )
        .unwrap(),
    );
    fixture
        .peer
        .fail_send
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let tracker = OperationTracker::new(fixture.journal.clone());
    let ticket = tracker
        .submit(
            &fixture.binding,
            command.clone(),
            Some(workspace()),
            &fixture.peer,
        )
        .await
        .unwrap();
    fixture
        .peer
        .results
        .lock()
        .unwrap()
        .push((ticket.id(), OperationLookup::NeverAdmitted));
    fixture
        .peer
        .fail_send
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let tracker = OperationTracker::new(fixture.reopen().await);
    tracker
        .reconcile(&fixture.binding, &fixture.peer)
        .await
        .unwrap();
    assert_eq!(fixture.peer.sent.lock().unwrap()[1].command(), &command);
    assert_eq!(
        fixture.peer.sent.lock().unwrap()[1].binding(),
        ticket.binding()
    );
}

#[tokio::test]
async fn cancelled_caller_cannot_lose_uncertain_commit_latch_after_provider_panic() {
    let fixture = Fixture::new().await;
    let gate = std::sync::Arc::new(CommitGate {
        panic_after_commit: true,
        storage: fixture.storage.clone(),
        entered: tokio::sync::Notify::new(),
        resume: tokio::sync::Notify::new(),
    });
    let journal = std::sync::Arc::new(
        spaceterm_client::journal::JournalController::recover(
            gate.clone(),
            fixture.codec.clone(),
            std::sync::Arc::new(Verifier),
            spaceterm_client::journal::JournalLimits::default(),
        )
        .await
        .unwrap(),
    );
    let tracker = OperationTracker::new(journal.clone());
    let binding = fixture.binding.clone();
    let caller = tokio::spawn(async move {
        tracker
            .submit(&binding, rename(1), Some(workspace()), &Peer::default())
            .await
    });
    gate.entered.notified().await;
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    gate.resume.notify_one();
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    assert_eq!(
        journal.ensure_healthy().unwrap_err().class(),
        ClientFailureClass::RecoveryRequired
    );
    assert_eq!(fixture.storage.state().operations().len(), 1);
}

#[tokio::test]
async fn consumed_confirmed_ticket_does_not_retain_outcome_during_new_admission() {
    let fixture = Fixture::new().await;
    let tracker = OperationTracker::new(fixture.journal.clone());
    let old = tracker
        .submit(
            &fixture.binding,
            rename(1),
            Some(workspace()),
            &fixture.peer,
        )
        .await
        .unwrap();
    tracker
        .resolve(old.id(), completed(5), &fixture.peer)
        .await
        .unwrap();
    tracker
        .confirm_acknowledgement(old.id().stream(), 1)
        .await
        .unwrap();
    fixture
        .journal
        .transact(vec![ClientJournalChange::ConsumeResult(old.id())])
        .await
        .unwrap();
    tracker
        .submit(
            &fixture.binding,
            rename(1),
            Some(workspace()),
            &fixture.peer,
        )
        .await
        .unwrap();
    assert!(matches!(*old.status().borrow(), OperationStatus::Retired));
}

#[path = "operation_recovery/retirement.rs"]
mod retirement;

#[path = "operation_recovery/ticket_publication.rs"]
mod ticket_publication;

#[path = "operation_recovery/pairing_projection.rs"]
mod pairing_projection;
