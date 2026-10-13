#![cfg(all(
    feature = "test-support",
    any(target_os = "macos", target_os = "linux")
))]

use spaceterm_host::{
    account_service::{ServiceFailure, ServiceSupervisor, test_support::AccountServiceFixture},
    native_deployment::contracts::NativeRequestId,
    startup::{BuildIdentity, DesiredServerState},
};
use std::{
    fs,
    os::unix::fs::{PermissionsExt as _, symlink},
    sync::Arc,
};

#[test]
fn passive_discovery_never_creates_storage_or_an_identity() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    assert!(fixture.reopen().discover().unwrap().is_none());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}

#[test]
fn request_reservation_precedes_registration_and_creates_no_key_or_service() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let request = fixture.reopen().allocate_request().unwrap();
    assert_ne!(request.as_bytes(), &[0; 16]);
    let entries: std::collections::BTreeSet<_> = fs::read_dir(fixture.anchor())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(
        entries,
        [
            ".account-mutation",
            ".account-mutation.complete",
            "requests"
        ]
        .into_iter()
        .map(std::ffi::OsString::from)
        .collect()
    );
    assert!(fixture.anchor().join("requests").is_file());
    assert!(!fixture.data_directory().exists());
    assert!(fixture.reopen().discover().unwrap().is_none());
}

#[test]
fn reconstructed_providers_reserve_monotonically_without_reusing_identifiers() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let first = fixture.reopen().allocate_request().unwrap();
    let second = fixture.reopen().allocate_request().unwrap();
    assert_eq!(&first.as_bytes()[..8], &second.as_bytes()[..8]);
    assert_eq!(
        u64::from_be_bytes(first.as_bytes()[8..].try_into().unwrap()),
        1
    );
    assert_eq!(
        u64::from_be_bytes(second.as_bytes()[8..].try_into().unwrap()),
        2
    );
}

#[test]
fn concurrent_independent_providers_converge_on_one_request_namespace() {
    let root = tempfile::tempdir().unwrap();
    let fixture =
        Arc::new(AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap());
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let fixture = Arc::clone(&fixture);
            std::thread::spawn(move || {
                (0..16)
                    .map(|_| *fixture.reopen().allocate_request().unwrap().as_bytes())
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    let ids: Vec<_> = threads
        .into_iter()
        .flat_map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(
        ids.iter().collect::<std::collections::BTreeSet<_>>().len(),
        64
    );
    assert!(ids.iter().all(|id| id[..8] == ids[0][..8]));
    let high_water = ids
        .iter()
        .map(|id| u64::from_be_bytes(id[8..].try_into().unwrap()))
        .max()
        .unwrap();
    assert_eq!(high_water, 64);
}

#[test]
fn unacknowledged_outcome_ceiling_rejects_before_further_effects() {
    use spaceterm_core::{identity::ServerId, operations::RequestDigest};
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let server = ServerId::new(registration.server_pin().identity);
    let claim = RequestDigest::from_bytes([5; 32]);
    let provider = fixture.reopen();
    let mut retained = Vec::new();
    for _ in 0..256 {
        let request = provider.allocate_request().unwrap();
        let digest = provider
            .owner_enrollment_digest(request, server, claim)
            .unwrap();
        provider
            .record_owner_enrollment(request, digest, server, claim)
            .unwrap();
        retained.push((request, digest));
    }
    let before = fs::read(fixture.anchor().join("requests")).unwrap();
    let identity = fs::read(fixture.data_directory().join("identity")).unwrap();
    assert_eq!(
        provider.allocate_request(),
        Err(ServiceFailure::LimitExceeded)
    );
    assert_eq!(fs::read(fixture.anchor().join("requests")).unwrap(), before);
    assert_eq!(
        fs::read(fixture.data_directory().join("identity")).unwrap(),
        identity
    );
    // No accepted result was silently retired to make room for another action.
    for (request, digest) in retained {
        fixture
            .reopen()
            .validate_owner_enrollment(&request, digest, server, claim)
            .unwrap();
    }
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
fn unknown_identifier_is_not_fresh_authority() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    fixture.reopen().allocate_request().unwrap();
    assert_eq!(
        fixture
            .reopen()
            .acknowledge_request(NativeRequestId::new([9; 16]).unwrap()),
        Err(ServiceFailure::UnknownRequest)
    );
}

#[test]
fn unsubmitted_reservation_cannot_be_acknowledged_as_a_completed_effect() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let request = fixture.reopen().allocate_request().unwrap();
    let before = fs::read(fixture.anchor().join("requests")).unwrap();
    assert_eq!(
        fixture.reopen().acknowledge_request(request),
        Err(ServiceFailure::Pending)
    );
    assert_eq!(fs::read(fixture.anchor().join("requests")).unwrap(), before);
}

#[test]
fn all_three_identities_have_disjoint_registration_data_and_private_routes() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let mut anchors = std::collections::BTreeSet::new();
    let mut directories = std::collections::BTreeSet::new();
    let mut endpoints = std::collections::BTreeSet::new();
    for build in [
        BuildIdentity::Release,
        BuildIdentity::Preflight,
        BuildIdentity::Development,
    ] {
        let fixture = AccountServiceFixture::new(root.path(), build).unwrap();
        let registered = fixture.initialize(DesiredServerState::Stopped).unwrap();
        anchors.insert(fixture.anchor().to_owned());
        directories.insert(registered.data_directory().to_owned());
        endpoints.insert(registered.endpoint().unwrap().path().to_owned());
        assert_eq!(registered.key().build_identity, build);
    }
    assert_eq!(anchors.len(), 3);
    assert_eq!(directories.len(), 3);
    assert_eq!(endpoints.len(), 3);
}

#[test]
fn established_stopped_intent_is_not_overwritten_by_registration_reuse() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let original = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let registration = fs::read(fixture.anchor().join("registration")).unwrap();
    drop(original);
    let reused = fixture.initialize(DesiredServerState::Running).unwrap();
    assert_eq!(reused.desired_state(), DesiredServerState::Stopped);
    assert_eq!(
        fs::read(fixture.anchor().join("registration")).unwrap(),
        registration
    );
}

#[test]
fn invalid_registration_schema_is_bounded_and_never_repaired() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    fixture.initialize(DesiredServerState::Stopped).unwrap();
    let path = fixture.anchor().join("registration");
    let bytes = fs::read(&path).unwrap();
    let text = String::from_utf8(bytes)
        .unwrap()
        .replace("\"schema\":1", "\"schema\":7");
    fs::write(&path, &text).unwrap();
    assert_eq!(
        fixture.reopen().discover().unwrap_err(),
        ServiceFailure::UnsupportedSchema
    );
    assert_eq!(fs::read(path).unwrap(), text.as_bytes());
}

#[test]
fn oversized_registration_never_triggers_initialization() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    fixture.initialize(DesiredServerState::Stopped).unwrap();
    let path = fixture.anchor().join("registration");
    fs::write(&path, vec![b' '; 32 * 1024 + 1]).unwrap();
    assert_eq!(
        fixture.reopen().discover().unwrap_err(),
        ServiceFailure::LimitExceeded
    );
    assert_eq!(fs::metadata(path).unwrap().len(), 32 * 1024 + 1);
}

#[test]
fn unsafe_registration_directory_is_rejected_without_permission_repair() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    fixture.reopen().allocate_request().unwrap();
    fs::set_permissions(fixture.anchor(), fs::Permissions::from_mode(0o750)).unwrap();
    assert_eq!(
        fixture.reopen().allocate_request(),
        Err(ServiceFailure::UnsafeStorage)
    );
    use std::os::unix::fs::MetadataExt as _;
    assert_eq!(
        fs::metadata(fixture.anchor()).unwrap().mode() & 0o777,
        0o750
    );
}

#[test]
fn symlink_registration_anchor_is_not_followed_or_modified() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    fixture.reopen().allocate_request().unwrap();
    let moved = fixture.anchor().with_extension("moved");
    fs::rename(fixture.anchor(), &moved).unwrap();
    symlink(&moved, fixture.anchor()).unwrap();
    let before = fs::read(moved.join("requests")).unwrap();
    assert_eq!(
        fixture.reopen().allocate_request(),
        Err(ServiceFailure::UnsafeStorage)
    );
    assert_eq!(fs::read(moved.join("requests")).unwrap(), before);
}

#[test]
fn current_provider_uses_actual_uid_and_compiled_build_identity() {
    let provider = ServiceSupervisor::current(None).unwrap();
    assert_eq!(provider.key().account_uid, unsafe { libc::geteuid() });
    let compiled = match env!("SPACETERM_HOST_BUILD") {
        "release" => BuildIdentity::Release,
        "preflight" => BuildIdentity::Preflight,
        "development" => BuildIdentity::Development,
        other => panic!("invalid compiled build identity {other}"),
    };
    assert_eq!(provider.key().build_identity, compiled);
}

#[test]
fn two_processes_reserve_one_namespace_with_no_reused_sequences() {
    const ROOT: &str = "SPACETERM_REQUEST_PROCESS_FIXTURE";
    const OUTPUT: &str = "SPACETERM_REQUEST_PROCESS_OUTPUT";
    if let Some(root) = std::env::var_os(ROOT) {
        let fixture =
            AccountServiceFixture::new(std::path::Path::new(&root), BuildIdentity::Development)
                .unwrap();
        let ids: Vec<String> = (0..16)
            .map(|_| fixture.reopen().allocate_request().unwrap().into())
            .collect();
        fs::write(
            std::env::var_os(OUTPUT).unwrap(),
            serde_json::to_vec(&ids).unwrap(),
        )
        .unwrap();
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let paths = [
        root.path().join("first-results"),
        root.path().join("second-results"),
    ];
    let mut children: Vec<_> = paths
        .iter()
        .map(|output| {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "two_processes_reserve_one_namespace_with_no_reused_sequences",
                    "--nocapture",
                ])
                .env(ROOT, root.path())
                .env(OUTPUT, output);
            spaceterm_host::process::spawn(&mut command).unwrap()
        })
        .collect();
    let statuses: Vec<_> = children
        .iter_mut()
        .map(|child| child.wait().unwrap())
        .collect();
    assert!(statuses.iter().all(std::process::ExitStatus::success));
    let ids: Vec<NativeRequestId> = paths
        .iter()
        .flat_map(|path| {
            serde_json::from_slice::<Vec<NativeRequestId>>(&fs::read(path).unwrap()).unwrap()
        })
        .collect();
    assert_eq!(ids.len(), 32);
    assert_eq!(
        ids.iter()
            .map(|id| *id.as_bytes())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        32
    );
    assert!(
        ids.iter()
            .all(|id| id.as_bytes()[..8] == ids[0].as_bytes()[..8])
    );
    let sequences: std::collections::BTreeSet<_> = ids
        .iter()
        .map(|id| u64::from_be_bytes(id.as_bytes()[8..].try_into().unwrap()))
        .collect();
    assert_eq!(sequences, (1..=32).collect());
}

#[test]
fn uncertain_initial_directory_creation_does_not_reopen_as_a_fresh_anchor() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    spaceterm_host::account_service::test_support::fail_next_creation_parent_sync();
    assert_eq!(
        fixture.reopen().allocate_request(),
        Err(ServiceFailure::Indeterminate)
    );
    assert_eq!(
        fixture.reopen().allocate_request(),
        Err(ServiceFailure::Indeterminate)
    );
    assert_eq!(
        fixture.reopen().discover().unwrap_err(),
        ServiceFailure::Indeterminate
    );
    assert!(!fixture.anchor().join("requests").exists());
    assert!(!fixture.data_directory().exists());
}

#[test]
fn initial_directory_uncertainty_survives_a_fresh_process() {
    const ROOT: &str = "SPACETERM_UNCERTAIN_DIRECTORY_FIXTURE";
    if let Some(root) = std::env::var_os(ROOT) {
        let fixture =
            AccountServiceFixture::new(std::path::Path::new(&root), BuildIdentity::Development)
                .unwrap();
        assert_eq!(
            fixture.reopen().allocate_request(),
            Err(ServiceFailure::Indeterminate)
        );
        assert_eq!(
            fixture.reopen().discover().unwrap_err(),
            ServiceFailure::Indeterminate
        );
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    spaceterm_host::account_service::test_support::fail_next_creation_parent_sync();
    assert_eq!(
        fixture.reopen().allocate_request(),
        Err(ServiceFailure::Indeterminate)
    );
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "initial_directory_uncertainty_survives_a_fresh_process",
            "--nocapture",
        ])
        .env(ROOT, root.path());
    let mut child = spaceterm_host::process::spawn(&mut command).unwrap();
    assert!(child.wait().unwrap().success());
    assert!(!fixture.anchor().join("requests").exists());
}

#[test]
fn malformed_mutation_phase_is_never_repaired_during_discovery_or_reservation() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let provider = fixture.reopen();
    provider.allocate_request().unwrap();
    let path = fixture.anchor().join(".account-mutation");
    fs::write(&path, b"unsupported or interrupted phase").unwrap();
    let before = fs::read(&path).unwrap();
    assert_eq!(
        fixture.reopen().discover().unwrap_err(),
        ServiceFailure::Indeterminate
    );
    assert_eq!(
        fixture.reopen().allocate_request(),
        Err(ServiceFailure::Indeterminate)
    );
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn missing_mutation_phase_cannot_admit_an_existing_request_record() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    fixture.reopen().allocate_request().unwrap();
    fs::remove_file(fixture.anchor().join(".account-mutation")).unwrap();
    let before = fs::read(fixture.anchor().join("requests")).unwrap();
    assert_eq!(
        fixture.reopen().allocate_request(),
        Err(ServiceFailure::Indeterminate)
    );
    assert_eq!(fs::read(fixture.anchor().join("requests")).unwrap(), before);
}

#[test]
fn hardlinked_mutation_phase_is_not_accepted_as_storage_authority() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    fixture.reopen().allocate_request().unwrap();
    let phase = fixture.anchor().join(".account-mutation");
    fs::hard_link(&phase, root.path().join("linked-phase")).unwrap();
    let before = fs::read(fixture.anchor().join("requests")).unwrap();
    assert_eq!(
        fixture.reopen().allocate_request(),
        Err(ServiceFailure::UnsafeStorage)
    );
    assert_eq!(fs::read(fixture.anchor().join("requests")).unwrap(), before);
}

#[test]
fn symlink_mutation_phase_is_not_followed_or_repaired() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    fixture.reopen().allocate_request().unwrap();
    let phase = fixture.anchor().join(".account-mutation");
    let unrelated = root.path().join("unrelated-phase");
    fs::rename(&phase, &unrelated).unwrap();
    symlink(&unrelated, &phase).unwrap();
    let before = fs::read(&unrelated).unwrap();
    assert_eq!(
        fixture.reopen().allocate_request(),
        Err(ServiceFailure::UnsafeStorage)
    );
    assert_eq!(fs::read(unrelated).unwrap(), before);
    assert!(
        fs::symlink_metadata(phase)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn directory_created_after_admission_precheck_cannot_escape_its_parent_fence() {
    const ROOT: &str = "SPACETERM_DIRECTORY_ADMISSION_INITIALIZER";
    if let Some(root) = std::env::var_os(ROOT) {
        let initializer =
            AccountServiceFixture::new(std::path::Path::new(&root), BuildIdentity::Development)
                .unwrap();
        spaceterm_host::account_service::test_support::fail_next_creation_parent_sync();
        assert_eq!(
            initializer.reopen().allocate_request(),
            Err(ServiceFailure::Indeterminate)
        );
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    fixture.reopen().allocate_request().unwrap();
    // Retain the existing private parent and its completed initialization fence.
    fs::remove_dir_all(fixture.anchor()).unwrap();
    let anchor = fixture.anchor().to_owned();
    let initializer_root = root.path().to_owned();
    spaceterm_host::account_service::test_support::before_next_directory_open(&anchor, move || {
        // Process A is paused before opening the anchor. Process B creates it,
        // but cannot synchronize its parent. A must inspect that exact fence.
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "directory_created_after_admission_precheck_cannot_escape_its_parent_fence",
                "--nocapture",
            ])
            .env(ROOT, initializer_root);
        let mut child = spaceterm_host::process::spawn(&mut command).unwrap();
        assert!(child.wait().unwrap().success());
    });
    assert_eq!(
        fixture.reopen().allocate_request(),
        Err(ServiceFailure::Indeterminate)
    );
    assert!(!fixture.anchor().join("requests").exists());
    assert!(!fixture.data_directory().join("identity").exists());
}

#[test]
fn lost_allocation_responses_cannot_permanently_exhaust_admission() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let mut first = None;
    for _ in 0..256 {
        // No mutation future was ever submitted. Reconstruction also loses every
        // allocation observer, so these are not accepted service outcomes.
        let request = fixture.reopen().allocate_request().unwrap();
        first.get_or_insert(request);
    }
    assert!(fixture.reopen().allocate_request().is_ok());
    assert_eq!(
        fixture.reopen().owner_enrollment_digest(
            first.unwrap(),
            spaceterm_core::identity::ServerId::new([1; 16]),
            spaceterm_core::operations::RequestDigest::from_bytes([2; 32])
        ),
        Err(ServiceFailure::RequestRetired)
    );
    assert!(!fixture.data_directory().exists());
}

#[test]
fn a_readable_clean_byte_without_this_operations_checkpoint_is_not_recovery() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    fixture.reopen().allocate_request().unwrap();
    spaceterm_host::account_service::test_support::fail_next_mutation_completion_sync();
    assert_eq!(
        fixture.reopen().allocate_request(),
        Err(ServiceFailure::Indeterminate)
    );
    let path = fixture.anchor().join(".account-mutation");
    let mut record = fs::read(&path).unwrap();
    record[5] = 0;
    fs::write(&path, record).unwrap();
    // An old completion marker exists, but binds another nonce. Changing the
    // phase byte cannot make that checkpoint validate the interrupted operation.
    let before = fs::read(fixture.anchor().join("requests")).unwrap();
    assert_eq!(
        fixture.reopen().allocate_request(),
        Err(ServiceFailure::Indeterminate)
    );
    assert_eq!(fs::read(fixture.anchor().join("requests")).unwrap(), before);
}

#[test]
fn explicit_original_directory_reconciliation_never_creates_missing_children() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    spaceterm_host::account_service::test_support::fail_next_creation_parent_sync();
    assert_eq!(
        fixture.reopen().allocate_request(),
        Err(ServiceFailure::Indeterminate)
    );
    assert_eq!(
        fixture.reopen().allocate_request(),
        Err(ServiceFailure::Indeterminate)
    );
    assert!(!fixture.anchor().exists());
    fixture.reconcile_initialization().unwrap();
    assert!(!fixture.anchor().exists());
    let request = fixture.reopen().allocate_request().unwrap();
    assert_ne!(request.as_bytes(), &[0; 16]);
    assert!(!fixture.data_directory().exists());
}

#[test]
fn pending_initialization_before_mkdir_reconciles_without_creating_the_child() {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    spaceterm_host::account_service::test_support::fail_next_creation_before_effect();
    assert_eq!(
        fixture.reopen().allocate_request(),
        Err(ServiceFailure::Indeterminate)
    );
    assert!(!fixture.anchor().parent().unwrap().exists());
    assert_eq!(
        fixture.reopen().allocate_request(),
        Err(ServiceFailure::Indeterminate)
    );
    fixture.reconcile_initialization().unwrap();
    assert!(!fixture.anchor().parent().unwrap().exists());
    fixture.reopen().allocate_request().unwrap();
    assert!(!fixture.data_directory().exists());
}

#[test]
fn original_owner_archive_publication_recovers_without_reissuing_authority() {
    use spaceterm_core::{identity::ServerId, operations::RequestDigest};
    for before_effect in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
        let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
        let server = ServerId::new(registration.server_pin().identity);
        let claim = RequestDigest::from_bytes([23; 32]);
        let supervisor = fixture.reopen();
        let request = supervisor.allocate_request().unwrap();
        let digest = supervisor
            .owner_enrollment_digest(request, server, claim)
            .unwrap();
        supervisor
            .record_owner_enrollment(request, digest, server, claim)
            .unwrap();
        if before_effect {
            spaceterm_host::account_service::test_support::fail_next_publication_before_effect();
        } else {
            spaceterm_host::account_service::test_support::fail_next_mutation_completion_sync();
        }
        assert_eq!(
            supervisor.acknowledge_request(request),
            Err(ServiceFailure::Indeterminate)
        );
        drop(supervisor);
        let identity = fs::read(fixture.data_directory().join("identity")).unwrap();
        assert_eq!(
            fixture.reopen().discover().unwrap_err(),
            ServiceFailure::Indeterminate
        );
        fixture.reopen().acknowledge_request(request).unwrap();
        fixture
            .reopen()
            .validate_owner_enrollment(&request, digest, server, claim)
            .unwrap();
        assert_eq!(
            fs::read(fixture.data_directory().join("identity")).unwrap(),
            identity
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
}
