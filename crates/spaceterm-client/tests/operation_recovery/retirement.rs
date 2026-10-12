use super::*;

#[tokio::test]
async fn retirement_of_one_identity_prevents_resubmission_of_its_stream() {
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
        .resolve(
            first.id(),
            OperationLookup::AcknowledgedRetired,
            &fixture.peer,
        )
        .await
        .unwrap();
    fixture.peer.sent.lock().unwrap().clear();

    assert_eq!(
        tracker
            .resolve(second.id(), OperationLookup::NeverAdmitted, &fixture.peer)
            .await
            .unwrap_err()
            .class(),
        ClientFailureClass::AuthorizationDenied
    );
    assert!(fixture.peer.sent.lock().unwrap().is_empty());
    assert!(
        fixture
            .journal
            .snapshot()
            .unwrap()
            .operation(second.id())
            .is_some()
    );
}

#[tokio::test]
async fn pending_lookup_cannot_replace_a_durable_completed_result() {
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
        .resolve(ticket.id(), completed(7), &fixture.peer)
        .await
        .unwrap();

    assert_eq!(
        tracker
            .resolve(ticket.id(), OperationLookup::Pending, &fixture.peer)
            .await
            .unwrap_err()
            .class(),
        ClientFailureClass::InvalidPeer
    );
    assert!(matches!(
        *ticket.status().borrow(),
        OperationStatus::Completed { publication_revision, .. }
            if publication_revision == OrganizationRevision::new(7)
    ));
}

#[tokio::test]
async fn lost_ack_on_retired_stream_restarts_with_durable_prefix_and_compacts_result() {
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
    fixture
        .peer
        .fail_ack
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        tracker
            .resolve(first.id(), completed(7), &fixture.peer)
            .await
            .unwrap_err()
            .class(),
        ClientFailureClass::Unavailable
    );
    tracker
        .resolve(
            second.id(),
            OperationLookup::AcknowledgedRetired,
            &fixture.peer,
        )
        .await
        .unwrap();
    fixture
        .journal
        .transact(vec![ClientJournalChange::ConsumeResult(first.id())])
        .await
        .unwrap();
    assert!(fixture.peer.acks.lock().unwrap().is_empty());

    let journal = fixture.reopen().await;
    let tracker = OperationTracker::new(journal.clone());
    tracker
        .reconcile(&fixture.binding, &fixture.peer)
        .await
        .unwrap();
    assert_eq!(
        fixture.peer.acks.lock().unwrap().as_slice(),
        &[(first.id().stream(), 1)]
    );
    assert_eq!(fixture.peer.sent.lock().unwrap().len(), 2);
    tracker
        .confirm_acknowledgement(first.id().stream(), 1)
        .await
        .unwrap();
    let retained = journal.snapshot().unwrap();
    assert!(retained.operation(first.id()).is_none());
    assert!(retained.stream(first.id().stream()).unwrap().retired);
    assert_eq!(
        retained
            .stream(first.id().stream())
            .unwrap()
            .confirmed_prefix,
        1
    );
    assert!(matches!(
        *tracker.ticket(second.id()).unwrap().status().borrow(),
        OperationStatus::Retired
    ));
    assert!(
        fixture
            .reopen()
            .await
            .snapshot()
            .unwrap()
            .operation(first.id())
            .is_none()
    );
}

#[tokio::test]
async fn field_conflict_cannot_retarget_the_retained_draft() {
    use spaceterm_core::{
        authority::FieldVersion,
        identity::WorkspaceId,
        operations::FieldConflict,
        organization::{NameMode, NameText, WorkspaceName},
    };
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
    let result = tracker
        .resolve(
            ticket.id(),
            OperationLookup::Completed {
                result: Outcome::Conflict(FieldConflict::WorkspaceName {
                    workspace: WorkspaceId::new(server(), 2).unwrap(),
                    current_value: WorkspaceName {
                        mode: NameMode::Custom,
                        display: NameText::new("another Workspace".into()).unwrap(),
                    },
                    current_version: FieldVersion::new(2),
                }),
                publication_revision: OrganizationRevision::new(3),
            },
            &fixture.peer,
        )
        .await;
    assert_eq!(result.unwrap_err().class(), ClientFailureClass::InvalidPeer);
    assert!(fixture.peer.acks.lock().unwrap().is_empty());
    assert!(tracker.conflict(ticket.id()).unwrap().is_none());
}
