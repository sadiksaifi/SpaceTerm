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
async fn empty_server_pairing_assessment_completion_is_durable_at_revision_zero() {
    use spaceterm_core::{
        authority::FieldVersion,
        operations::{AssessmentRef, Command, OperationLookup, Outcome},
        pairing::{
            PairingCommand, PairingOutcome, PairingTarget, PairingTargetParts, UnpairAssessment,
            UnpairAssessmentParts,
        },
    };
    let fixture = durable::Fixture::new().await;
    let target = fixture.binding.target();
    let pairing = PairingTarget::new(PairingTargetParts {
        server: target.server(),
        pairing: target.pairing(),
        device: target.device(),
    })
    .unwrap();
    let tracker = OperationTracker::new(fixture.journal.clone());
    let ticket = tracker
        .submit(
            &fixture.binding,
            Command::Pairing(PairingCommand::AssessUnpair {
                target: pairing.clone(),
            }),
            None,
            &fixture.peer,
        )
        .await
        .unwrap();
    let outcome = Outcome::Pairing(Box::new(PairingOutcome::UnpairAssessed(
        UnpairAssessment::new(UnpairAssessmentParts {
            reference: AssessmentRef {
                operation: ticket.id(),
                digest: ticket.binding().digest,
            },
            target: pairing,
            authorization_version: FieldVersion::new(1),
            clients: vec![spaceterm_core::pairing::ClientRecord {
                id: target.client(),
                pairing: target.pairing(),
            }],
        })
        .unwrap(),
    )));
    tracker
        .resolve_bound(
            ticket.binding(),
            OperationLookup::Completed {
                result: outcome.clone(),
                publication_revision: OrganizationRevision::new(0),
            },
            &fixture.peer,
        )
        .await
        .unwrap();
    let reopened = fixture.reopen().await;
    let record = reopened
        .snapshot()
        .unwrap()
        .operation(ticket.id())
        .unwrap()
        .clone();
    let spaceterm_client::journal::RetainedOperationState::Completed {
        outcome: retained,
        publication_revision,
        ..
    } = record.state
    else {
        panic!("pairing completion must survive journal recovery");
    };
    assert_eq!(retained.outcome(fixture.codec.as_ref()).unwrap(), outcome);
    assert_eq!(publication_revision, OrganizationRevision::new(0));
}

#[tokio::test]
async fn organization_mutation_completion_still_requires_nonzero_revision() {
    use spaceterm_core::operations::{OperationLookup, Outcome};
    let fixture = durable::Fixture::new().await;
    let tracker = OperationTracker::new(fixture.journal.clone());
    let ticket = tracker
        .submit(
            &fixture.binding,
            durable::rename(1),
            Some(durable::workspace()),
            &fixture.peer,
        )
        .await
        .unwrap();
    let revision = fixture.journal.snapshot().unwrap().revision();
    let error = tracker
        .resolve_bound(
            ticket.binding(),
            OperationLookup::Completed {
                result: Outcome::Applied,
                publication_revision: OrganizationRevision::new(0),
            },
            &fixture.peer,
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.class(),
        spaceterm_client::failure::ClientFailureClass::InvalidStorage
    );
    assert_eq!(fixture.journal.snapshot().unwrap().revision(), revision);
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
