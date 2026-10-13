#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::{fs, sync::Arc};

use spaceterm_core::{
    identity::{DeviceId, ServerId},
    operations::RequestDigest,
    pairing::{
        EnrollmentProposal, EnrollmentProposalParts, PairingFailure, PairingKeys, PairingKeysParts,
    },
};
use spaceterm_host::{
    account_service::test_support::AccountServiceFixture,
    startup::{BuildIdentity, DesiredServerState},
};
use spaceterm_protocol::pairing_adapter::enrollment_proposal_digest;
use spaceterm_server::{
    account_startup::AccountOwnerEnrollment, pairing::management::AccountOwnerEnrollmentPort,
};

fn proposal(server: ServerId, nonce: u8) -> EnrollmentProposal {
    EnrollmentProposal::new(EnrollmentProposalParts {
        server,
        device: DeviceId::new([2; 16]),
        keys: PairingKeys::new(PairingKeysParts {
            device_identity_public_key: [3; 32],
            pairing_authentication_public_key: [4; 32],
        })
        .unwrap(),
        claim_nonce: [nonce; 32],
    })
    .unwrap()
}

#[test]
fn retained_owner_authority_validates_after_provider_reconstruction_without_reissuance() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let supervisor = Arc::new(fixture.reopen());
    let request = supervisor.allocate_request().unwrap();
    let proposal = proposal(server, 1);
    let port = AccountOwnerEnrollment::new(supervisor.clone());
    let digest = port.record_authorization(request, &proposal).unwrap();
    drop(port);
    drop(supervisor);

    let reconstructed = Arc::new(fixture.reopen());
    let port = AccountOwnerEnrollment::new(reconstructed.clone());
    let claim_digest = enrollment_proposal_digest(&proposal).unwrap();
    port.validate_owner_request(&request, digest, server, &proposal)
        .unwrap();
    port.revalidate_owner_issuance(&request, digest, server, claim_digest)
        .unwrap();
    // Validation never invokes Start or replaces the retained request authority.
    assert_eq!(
        reconstructed.discover().unwrap().unwrap().desired_state(),
        DesiredServerState::Stopped
    );
    reconstructed.acknowledge_request(request).unwrap();
    let port = AccountOwnerEnrollment::new(Arc::new(fixture.reopen()));
    port.validate_owner_request(&request, digest, server, &proposal)
        .unwrap();
    port.revalidate_owner_issuance(&request, digest, server, claim_digest)
        .unwrap();
}

#[test]
fn missing_owner_record_is_not_created_by_either_validation_callback() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let supervisor = Arc::new(fixture.reopen());
    let request = supervisor.allocate_request().unwrap();
    let proposal = proposal(server, 1);
    let claim_digest = enrollment_proposal_digest(&proposal).unwrap();
    let digest = supervisor
        .owner_enrollment_digest(request, server, claim_digest)
        .unwrap();
    let port = AccountOwnerEnrollment::new(supervisor.clone());
    for _ in 0..2 {
        assert_eq!(
            port.validate_owner_request(&request, digest, server, &proposal),
            Err(PairingFailure::AuthorizationRequired)
        );
        assert_eq!(
            port.revalidate_owner_issuance(&request, digest, server, claim_digest),
            Err(PairingFailure::AuthorizationRequired)
        );
    }
    assert!(
        supervisor
            .validate_owner_enrollment(&request, digest, server, claim_digest)
            .is_err()
    );
}

#[test]
fn owner_authority_rejects_changed_claim_server_or_request_digest() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let port = AccountOwnerEnrollment::new(Arc::new(fixture.reopen()));
    let request = fixture.reopen().allocate_request().unwrap();
    let proposal = proposal(server, 1);
    let digest = port.record_authorization(request, &proposal).unwrap();
    let changed_claim = self::proposal(server, 2);
    assert!(
        port.validate_owner_request(&request, digest, server, &changed_claim)
            .is_err()
    );
    assert!(
        port.validate_owner_request(
            &request,
            RequestDigest::from_bytes([9; 32]),
            server,
            &proposal
        )
        .is_err()
    );
    assert!(
        port.validate_owner_request(&request, digest, ServerId::new([8; 16]), &proposal)
            .is_err()
    );
    port.validate_owner_request(&request, digest, server, &proposal)
        .unwrap();
}

#[test]
fn owner_issuance_retries_original_claim_but_cannot_replace_it() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let supervisor = Arc::new(fixture.reopen());
    let request = supervisor.allocate_request().unwrap();
    let port = AccountOwnerEnrollment::new(supervisor.clone());
    let original = proposal(server, 1);
    let digest = port.record_authorization(request, &original).unwrap();
    let retained = fs::read(fixture.anchor().join("requests")).unwrap();

    assert_eq!(
        port.record_authorization(request, &original).unwrap(),
        digest
    );
    assert_eq!(
        port.record_authorization(request, &proposal(server, 2)),
        Err(PairingFailure::RequestIdentityReused)
    );
    assert_eq!(
        fs::read(fixture.anchor().join("requests")).unwrap(),
        retained
    );
    port.validate_owner_request(&request, digest, server, &original)
        .unwrap();
    let current = supervisor.discover().unwrap().unwrap();
    assert_eq!(current.server_pin(), registration.server_pin());
    assert_eq!(current.generation(), registration.generation());
    assert_eq!(current.desired_state(), DesiredServerState::Stopped);
}

#[test]
fn foreign_build_cannot_consume_original_owner_request_or_create_authority() {
    let root = tempfile::tempdir().unwrap();
    let original = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = original.initialize(DesiredServerState::Stopped).unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let request = original.reopen().allocate_request().unwrap();
    let claim = proposal(server, 1);
    let issuer = AccountOwnerEnrollment::new(Arc::new(original.reopen()));
    let digest = issuer.record_authorization(request, &claim).unwrap();
    let other = AccountServiceFixture::new(root.path(), BuildIdentity::Preflight).unwrap();
    let foreign = AccountOwnerEnrollment::new(Arc::new(other.reopen()));

    assert_eq!(
        foreign.validate_owner_request(&request, digest, server, &claim),
        Err(PairingFailure::AuthorizationRequired)
    );
    assert_eq!(
        foreign.revalidate_owner_issuance(
            &request,
            digest,
            server,
            enrollment_proposal_digest(&claim).unwrap()
        ),
        Err(PairingFailure::AuthorizationRequired)
    );
    assert!(!other.anchor().exists());
    assert!(!other.data_directory().exists());
    issuer
        .validate_owner_request(&request, digest, server, &claim)
        .unwrap();
}

#[test]
fn unavailable_owner_journal_fails_validation_without_repair_or_reissuance() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let request = fixture.reopen().allocate_request().unwrap();
    let claim = proposal(server, 1);
    let port = AccountOwnerEnrollment::new(Arc::new(fixture.reopen()));
    let digest = port.record_authorization(request, &claim).unwrap();
    let journal = fixture.anchor().join("requests");
    fs::remove_file(&journal).unwrap();
    let port = AccountOwnerEnrollment::new(Arc::new(fixture.reopen()));

    assert_eq!(
        port.validate_owner_request(&request, digest, server, &claim),
        Err(PairingFailure::AuthorizationRequired)
    );
    assert_eq!(
        port.revalidate_owner_issuance(
            &request,
            digest,
            server,
            enrollment_proposal_digest(&claim).unwrap()
        ),
        Err(PairingFailure::AuthorizationRequired)
    );
    assert!(!journal.exists());
    assert_eq!(
        fixture
            .reopen()
            .discover()
            .unwrap()
            .unwrap()
            .desired_state(),
        DesiredServerState::Stopped
    );
}
