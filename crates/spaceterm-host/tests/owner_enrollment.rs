#![cfg(all(
    feature = "test-support",
    any(target_os = "macos", target_os = "linux")
))]

use spaceterm_core::{identity::ServerId, operations::RequestDigest};
use spaceterm_host::{
    account_service::{ServiceFailure, test_support::AccountServiceFixture},
    startup::{BuildIdentity, DesiredServerState},
};
use std::fs;

#[test]
fn owner_authority_reconstructs_from_storage_without_an_in_memory_receipt() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let claim = RequestDigest::from_bytes([5; 32]);
    let provider = fixture.reopen();
    let request = provider.allocate_request().unwrap();
    let digest = provider
        .owner_enrollment_digest(request, server, claim)
        .unwrap();
    provider
        .record_owner_enrollment(request, digest, server, claim)
        .unwrap();
    drop(provider);
    drop(registration);
    let before = fs::read(fixture.anchor().join("requests")).unwrap();
    let registration_before = fs::read(fixture.anchor().join("registration")).unwrap();
    fixture
        .reopen()
        .validate_owner_enrollment(&request, digest, server, claim)
        .unwrap();
    assert_eq!(fs::read(fixture.anchor().join("requests")).unwrap(), before);
    assert_eq!(
        fs::read(fixture.anchor().join("registration")).unwrap(),
        registration_before
    );
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

#[test]
fn missing_owner_authority_fails_without_issuing_or_rewriting_it() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let claim = RequestDigest::from_bytes([5; 32]);
    let provider = fixture.reopen();
    let request = provider.allocate_request().unwrap();
    let digest = provider
        .owner_enrollment_digest(request, server, claim)
        .unwrap();
    let before = fs::read(fixture.anchor().join("requests")).unwrap();
    assert_eq!(
        provider.validate_owner_enrollment(&request, digest, server, claim),
        Err(ServiceFailure::Missing)
    );
    assert_eq!(fs::read(fixture.anchor().join("requests")).unwrap(), before);
}

#[test]
fn changed_claim_under_one_issued_id_is_a_conflict_and_never_rewrites_authority() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let claim = RequestDigest::from_bytes([5; 32]);
    let changed_claim = RequestDigest::from_bytes([6; 32]);
    let provider = fixture.reopen();
    let request = provider.allocate_request().unwrap();
    let original = provider
        .owner_enrollment_digest(request, server, claim)
        .unwrap();
    let changed = provider
        .owner_enrollment_digest(request, server, changed_claim)
        .unwrap();
    provider
        .record_owner_enrollment(request, original, server, claim)
        .unwrap();
    let before = fs::read(fixture.anchor().join("requests")).unwrap();
    assert_eq!(
        provider.record_owner_enrollment(request, changed, server, changed_claim),
        Err(ServiceFailure::RequestConflict)
    );
    assert_eq!(
        provider.validate_owner_enrollment(&request, changed, server, changed_claim),
        Err(ServiceFailure::RequestConflict)
    );
    assert_eq!(fs::read(fixture.anchor().join("requests")).unwrap(), before);
    provider
        .validate_owner_enrollment(&request, original, server, claim)
        .unwrap();
}

#[test]
fn canonical_owner_digest_is_checked_before_issuance() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let provider = fixture.reopen();
    let request = provider.allocate_request().unwrap();
    let before = fs::read(fixture.anchor().join("requests")).unwrap();
    assert_eq!(
        provider.record_owner_enrollment(
            request,
            RequestDigest::from_bytes([8; 32]),
            server,
            RequestDigest::from_bytes([5; 32])
        ),
        Err(ServiceFailure::InvalidRecord)
    );
    assert_eq!(fs::read(fixture.anchor().join("requests")).unwrap(), before);
}

#[test]
fn owner_issuance_is_bound_to_the_registered_server() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    fixture.initialize(DesiredServerState::Stopped).unwrap();
    let provider = fixture.reopen();
    let request = provider.allocate_request().unwrap();
    let foreign = ServerId::new([9; 16]);
    let claim = RequestDigest::from_bytes([5; 32]);
    let digest = provider
        .owner_enrollment_digest(request, foreign, claim)
        .unwrap();
    let before = fs::read(fixture.anchor().join("requests")).unwrap();
    assert_eq!(
        provider.record_owner_enrollment(request, digest, foreign, claim),
        Err(ServiceFailure::IdentityMismatch)
    );
    assert_eq!(fs::read(fixture.anchor().join("requests")).unwrap(), before);
}

#[test]
fn owner_authority_does_not_transfer_between_build_identities() {
    let root = tempfile::tempdir().unwrap();
    let development = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let preflight = AccountServiceFixture::new(root.path(), BuildIdentity::Preflight).unwrap();
    let registered = development.initialize(DesiredServerState::Stopped).unwrap();
    preflight.initialize(DesiredServerState::Stopped).unwrap();
    let provider = development.reopen();
    let request = provider.allocate_request().unwrap();
    let server = ServerId::new(registered.server_pin().identity);
    let claim = RequestDigest::from_bytes([5; 32]);
    let digest = provider
        .owner_enrollment_digest(request, server, claim)
        .unwrap();
    provider
        .record_owner_enrollment(request, digest, server, claim)
        .unwrap();
    assert_eq!(
        preflight
            .reopen()
            .validate_owner_enrollment(&request, digest, server, claim),
        Err(ServiceFailure::IdentityMismatch)
    );
}

#[test]
fn acknowledging_a_completed_owner_request_preserves_pairing_revalidation() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registered = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let server = ServerId::new(registered.server_pin().identity);
    let claim = RequestDigest::from_bytes([5; 32]);
    let provider = fixture.reopen();
    let request = provider.allocate_request().unwrap();
    let digest = provider
        .owner_enrollment_digest(request, server, claim)
        .unwrap();
    provider
        .record_owner_enrollment(request, digest, server, claim)
        .unwrap();
    provider.acknowledge_request(request).unwrap();
    drop(provider);
    fixture
        .reopen()
        .validate_owner_enrollment(&request, digest, server, claim)
        .unwrap();
}

#[test]
fn repeated_identical_issuance_preserves_the_original_file_version() {
    use std::os::unix::fs::MetadataExt as _;
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registered = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let server = ServerId::new(registered.server_pin().identity);
    let claim = RequestDigest::from_bytes([5; 32]);
    let provider = fixture.reopen();
    let request = provider.allocate_request().unwrap();
    let digest = provider
        .owner_enrollment_digest(request, server, claim)
        .unwrap();
    provider
        .record_owner_enrollment(request, digest, server, claim)
        .unwrap();
    let path = fixture.anchor().join("requests");
    let original_inode = fs::metadata(&path).unwrap().ino();
    let bytes = fs::read(&path).unwrap();
    provider
        .record_owner_enrollment(request, digest, server, claim)
        .unwrap();
    assert_eq!(fs::metadata(&path).unwrap().ino(), original_inode);
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn uncertain_owner_publication_is_not_authority_after_reconstruction() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let claim = RequestDigest::from_bytes([5; 32]);
    let provider = fixture.reopen();
    let other = fixture.reopen();
    let request = provider.allocate_request().unwrap();
    let digest = provider
        .owner_enrollment_digest(request, server, claim)
        .unwrap();
    spaceterm_host::account_service::test_support::fail_next_publication_durability();
    assert_eq!(
        provider.record_owner_enrollment(request, digest, server, claim),
        Err(ServiceFailure::Indeterminate)
    );
    let visible = fs::read(fixture.anchor().join("requests")).unwrap();
    assert_eq!(
        other.validate_owner_enrollment(&request, digest, server, claim),
        Err(ServiceFailure::Indeterminate)
    );
    drop(provider);
    assert_eq!(
        fixture
            .reopen()
            .validate_owner_enrollment(&request, digest, server, claim),
        Err(ServiceFailure::Indeterminate)
    );
    assert_eq!(
        fixture.reopen().allocate_request(),
        Err(ServiceFailure::Indeterminate)
    );
    assert_eq!(
        fs::read(fixture.anchor().join("requests")).unwrap(),
        visible
    );
}

#[test]
fn uncertain_owner_authority_remains_closed_in_a_fresh_process() {
    const ROOT: &str = "SPACETERM_UNCERTAIN_OWNER_FIXTURE";
    if let Some(root) = std::env::var_os(ROOT) {
        let root = std::path::Path::new(&root);
        let fixture = AccountServiceFixture::new(root, BuildIdentity::Development).unwrap();
        let (request, digest, server, claim) =
            serde_json::from_slice(&fs::read(root.join("public-inputs")).unwrap()).unwrap();
        assert_eq!(
            fixture
                .reopen()
                .validate_owner_enrollment(&request, digest, server, claim),
            Err(ServiceFailure::Indeterminate)
        );
        assert_eq!(
            fixture.reopen().allocate_request(),
            Err(ServiceFailure::Indeterminate)
        );
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let claim = RequestDigest::from_bytes([5; 32]);
    let provider = fixture.reopen();
    let request = provider.allocate_request().unwrap();
    let digest = provider
        .owner_enrollment_digest(request, server, claim)
        .unwrap();
    fs::write(
        root.path().join("public-inputs"),
        serde_json::to_vec(&(request, digest, server, claim)).unwrap(),
    )
    .unwrap();
    spaceterm_host::account_service::test_support::fail_next_publication_durability();
    assert_eq!(
        provider.record_owner_enrollment(request, digest, server, claim),
        Err(ServiceFailure::Indeterminate)
    );
    let original = fs::read(fixture.anchor().join("requests")).unwrap();
    drop(provider);
    drop(registration);
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "uncertain_owner_authority_remains_closed_in_a_fresh_process",
            "--nocapture",
        ])
        .env(ROOT, root.path());
    let mut child = spaceterm_host::process::spawn(&mut command).unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(
        fs::read(fixture.anchor().join("requests")).unwrap(),
        original
    );
}

#[test]
fn acknowledged_owner_results_release_admission_without_erasing_pairing_authority() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let claim = RequestDigest::from_bytes([5; 32]);
    let provider = fixture.reopen();
    let mut first = None;
    for _ in 0..256 {
        let request = provider.allocate_request().unwrap();
        let digest = provider
            .owner_enrollment_digest(request, server, claim)
            .unwrap();
        provider
            .record_owner_enrollment(request, digest, server, claim)
            .unwrap();
        provider.acknowledge_request(request).unwrap();
        first.get_or_insert((request, digest));
    }
    let reopened = fixture.reopen();
    assert!(
        reopened.allocate_request().is_ok(),
        "acknowledged completion must not impose a lifetime admission ceiling"
    );
    let (request, digest) = first.unwrap();
    reopened
        .validate_owner_enrollment(&request, digest, server, claim)
        .unwrap();
}

#[test]
fn owner_completion_file_sync_failure_retains_the_original_closed_phase() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let claim = RequestDigest::from_bytes([5; 32]);
    let provider = fixture.reopen();
    let request = provider.allocate_request().unwrap();
    let digest = provider
        .owner_enrollment_digest(request, server, claim)
        .unwrap();
    spaceterm_host::account_service::test_support::fail_next_mutation_completion_sync();
    assert_eq!(
        provider.record_owner_enrollment(request, digest, server, claim),
        Err(ServiceFailure::Indeterminate)
    );
    let before = fs::read(fixture.anchor().join("requests")).unwrap();
    assert_eq!(
        fixture
            .reopen()
            .validate_owner_enrollment(&request, digest, server, claim),
        Err(ServiceFailure::Indeterminate)
    );
    assert_eq!(fs::read(fixture.anchor().join("requests")).unwrap(), before);
}

#[test]
fn completion_sync_and_rollback_write_failure_cannot_reopen_owner_authority() {
    const ROOT: &str = "SPACETERM_COMPLETION_ROLLBACK_FIXTURE";
    if let Some(root) = std::env::var_os(ROOT) {
        let fixture =
            AccountServiceFixture::new(std::path::Path::new(&root), BuildIdentity::Development)
                .unwrap();
        let (request, digest, server, claim) = serde_json::from_slice(
            &fs::read(std::path::Path::new(&root).join("public-inputs")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            fixture
                .reopen()
                .validate_owner_enrollment(&request, digest, server, claim),
            Err(ServiceFailure::Indeterminate)
        );
        assert_eq!(
            fixture.reopen().allocate_request(),
            Err(ServiceFailure::Indeterminate)
        );
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let claim = RequestDigest::from_bytes([5; 32]);
    let provider = fixture.reopen();
    let other = fixture.reopen();
    let request = provider.allocate_request().unwrap();
    let digest = provider
        .owner_enrollment_digest(request, server, claim)
        .unwrap();
    fs::write(
        root.path().join("public-inputs"),
        serde_json::to_vec(&(request, digest, server, claim)).unwrap(),
    )
    .unwrap();
    spaceterm_host::account_service::test_support::fail_next_completion_and_rollback_write();
    assert_eq!(
        provider.record_owner_enrollment(request, digest, server, claim),
        Err(ServiceFailure::Indeterminate)
    );
    let pending = fs::read(fixture.anchor().join(".account-mutation")).unwrap();
    assert_eq!(
        pending[5], 1,
        "completion must not overwrite original Pending evidence"
    );
    assert_eq!(
        other.validate_owner_enrollment(&request, digest, server, claim),
        Err(ServiceFailure::Indeterminate)
    );
    drop(provider);
    assert_eq!(
        fixture
            .reopen()
            .validate_owner_enrollment(&request, digest, server, claim),
        Err(ServiceFailure::Indeterminate)
    );
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "completion_sync_and_rollback_write_failure_cannot_reopen_owner_authority",
            "--nocapture",
        ])
        .env(ROOT, root.path());
    let mut child = spaceterm_host::process::spawn(&mut command).unwrap();
    assert!(child.wait().unwrap().success());
}

#[test]
fn acknowledged_request_id_is_permanently_retired_but_owner_validation_remains_read_only() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registered = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let server = ServerId::new(registered.server_pin().identity);
    let claim = RequestDigest::from_bytes([5; 32]);
    let provider = fixture.reopen();
    let request = provider.allocate_request().unwrap();
    let digest = provider
        .owner_enrollment_digest(request, server, claim)
        .unwrap();
    provider
        .record_owner_enrollment(request, digest, server, claim)
        .unwrap();
    provider.acknowledge_request(request).unwrap();
    let before = fs::read(fixture.anchor().join("requests")).unwrap();
    let next = fixture.reopen().allocate_request().unwrap();
    assert_ne!(next, request);
    let retained = fs::read(fixture.anchor().join("requests")).unwrap();
    assert_ne!(retained, before);
    assert_eq!(
        fixture
            .reopen()
            .record_owner_enrollment(request, digest, server, claim),
        Err(ServiceFailure::RequestRetired)
    );
    assert_eq!(
        fixture
            .reopen()
            .owner_enrollment_digest(request, server, claim),
        Err(ServiceFailure::RequestRetired)
    );
    fixture
        .reopen()
        .validate_owner_enrollment(&request, digest, server, claim)
        .unwrap();
    fixture.reopen().acknowledge_request(request).unwrap();
    assert_eq!(
        fs::read(fixture.anchor().join("requests")).unwrap(),
        retained
    );
}

#[test]
fn interrupted_owner_archiving_keeps_admission_closed_without_erasing_issuance() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let claim = RequestDigest::from_bytes([5; 32]);
    let provider = fixture.reopen();
    let request = provider.allocate_request().unwrap();
    let digest = provider
        .owner_enrollment_digest(request, server, claim)
        .unwrap();
    provider
        .record_owner_enrollment(request, digest, server, claim)
        .unwrap();
    let before = fs::read(fixture.anchor().join("requests")).unwrap();
    spaceterm_host::account_service::test_support::fail_next_publication_durability();
    assert_eq!(
        provider.acknowledge_request(request),
        Err(ServiceFailure::Indeterminate)
    );
    assert_eq!(fs::read(fixture.anchor().join("requests")).unwrap(), before);
    assert_eq!(
        fixture
            .reopen()
            .validate_owner_enrollment(&request, digest, server, claim),
        Err(ServiceFailure::Indeterminate)
    );
    assert_eq!(
        fixture.reopen().allocate_request(),
        Err(ServiceFailure::Indeterminate)
    );
}

#[test]
fn original_public_owner_proposal_survives_restart_and_endpoint_revalidation() {
    use spaceterm_core::{
        identity::DeviceId,
        pairing::{EnrollmentProposal, EnrollmentProposalParts, PairingKeys, PairingKeysParts},
    };
    use spaceterm_protocol::pairing_adapter::enrollment_proposal_digest;
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let proposal = EnrollmentProposal::new(EnrollmentProposalParts {
        server,
        device: DeviceId::new([4; 16]),
        keys: PairingKeys::new(PairingKeysParts {
            device_identity_public_key: [5; 32],
            pairing_authentication_public_key: [6; 32],
        })
        .unwrap(),
        claim_nonce: [7; 32],
    })
    .unwrap();
    let provider = fixture.reopen();
    let request = provider.allocate_request().unwrap();
    provider
        .record_owner_proposal(request, proposal.clone())
        .unwrap();
    let claim = enrollment_proposal_digest(&proposal).unwrap();
    let digest = provider
        .owner_enrollment_digest(request, server, claim)
        .unwrap();
    drop(provider);
    let reopened = fixture.reopen();
    let before = fs::read(fixture.anchor().join("requests")).unwrap();
    assert_eq!(
        reopened
            .retained_owner_proposal(server, &proposal.parts().keys)
            .unwrap(),
        Some((request, proposal.clone()))
    );
    reopened
        .validate_owner_enrollment(&request, digest, server, claim)
        .unwrap();
    assert_eq!(fs::read(fixture.anchor().join("requests")).unwrap(), before);
    reopened
        .record_owner_enrollment(request, digest, server, claim)
        .unwrap();
    assert_eq!(
        reopened
            .retained_owner_proposal(server, &proposal.parts().keys)
            .unwrap(),
        Some((request, proposal.clone()))
    );
    let mut changed = proposal.parts().clone();
    changed.claim_nonce = [8; 32];
    assert_eq!(
        reopened.record_owner_proposal(request, EnrollmentProposal::new(changed).unwrap()),
        Err(ServiceFailure::RequestConflict)
    );
    assert_eq!(
        reopened.discover().unwrap().unwrap().desired_state(),
        DesiredServerState::Stopped
    );
}
