#[path = "operation_recovery/support.rs"]
mod durable;
mod support;

use spaceterm_client::{
    limits::ClientLimits,
    operations::OperationTracker,
    replica::{OrganizationResidency, Replica},
};
use spaceterm_core::authority::{
    ControlGeneration, ControlGrant, GrantedControl, OrganizationRevision, ServerEpoch,
};
use std::sync::Arc;

#[tokio::test]
async fn authenticated_empty_server_recovery_accepts_revision_zero_and_reopens() {
    let fixture = durable::Fixture::new().await;
    let server = fixture.binding.target().server();
    let mut replica = Replica::new(
        server,
        Arc::new(OrganizationResidency::new(ClientLimits::default()).unwrap()),
    );
    let token = replica.begin_resync(ServerEpoch::new(1)).unwrap();
    replica
        .install_snapshot(token, support::snapshot(server, 1, 0))
        .unwrap();
    let snapshot = replica.subscribe().borrow().retained().unwrap().clone();
    OperationTracker::new(fixture.journal.clone())
        .reconcile_grants(&fixture.binding, &snapshot)
        .await
        .unwrap();
    let reopened = fixture.reopen().await;
    assert!(reopened.snapshot().unwrap().grants().is_empty());
    assert!(reopened.ordinary_use_allowed(&fixture.binding).unwrap());
    OperationTracker::new(reopened)
        .reconcile_grants(&fixture.binding, &snapshot)
        .await
        .unwrap();
}

#[tokio::test]
async fn revision_zero_recovery_cannot_publish_a_control_grant() {
    let fixture = durable::Fixture::new().await;
    let grant = GrantedControl::new(
        ServerEpoch::new(1),
        ControlGrant::new(
            spaceterm_core::identity::PaneId::new(fixture.binding.target().server(), 1).unwrap(),
            fixture.binding.target().client(),
            ControlGeneration::new(1),
        )
        .unwrap(),
    )
    .unwrap();
    let revision = fixture.journal.snapshot().unwrap().revision();
    let error = fixture
        .journal
        .transact(vec![
            spaceterm_client::journal::ClientJournalChange::ReconcileGrants {
                local_pairing: fixture.binding.target().local_pairing().unwrap(),
                epoch: ServerEpoch::new(1),
                revision: OrganizationRevision::new(0),
                grants: vec![(durable::workspace(), grant)],
            },
        ])
        .await
        .unwrap_err();
    assert_eq!(
        error.class(),
        spaceterm_client::failure::ClientFailureClass::InvalidStorage
    );
    assert_eq!(fixture.journal.snapshot().unwrap().revision(), revision);
    assert!(fixture.journal.snapshot().unwrap().grants().is_empty());
}
