#![cfg(all(
    feature = "test-support",
    any(target_os = "macos", target_os = "linux")
))]

use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink},
    sync::{Arc, Barrier},
};

use ring::signature::{ED25519, UnparsedPublicKey};
use spaceterm_core::{
    identity::ServerId,
    pairing::{EnrollmentTicketId, EnrollmentTicketParts},
};
use spaceterm_host::{
    account_service::{ServiceFailure, test_support::AccountServiceFixture},
    server_keys::{KeyStoreFailure, PairingSecret, ServerKeyStore},
    startup::{BuildIdentity, DesiredServerState},
};
use spaceterm_protocol::authentication::SigningKey as _;
use zeroize::Zeroizing;

fn fixture() -> (tempfile::TempDir, AccountServiceFixture) {
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    (root, fixture)
}
fn ticket(server: [u8; 16], nonce: u8) -> EnrollmentTicketId {
    EnrollmentTicketId::new(EnrollmentTicketParts {
        server: ServerId::new(server),
        nonce: [nonce; 16],
    })
    .unwrap()
}
fn secret() -> PairingSecret {
    PairingSecret::new(Zeroizing::new(
        b"bearer body which must be redacted".to_vec(),
    ))
    .unwrap()
}
fn secret_path(fixture: &AccountServiceFixture, nonce: u8) -> std::path::PathBuf {
    fixture
        .data_directory()
        .join(format!("pairing-{}", format!("{nonce:02x}").repeat(16)))
}

#[test]
fn established_identity_reconstruction_retains_the_pin_and_signs() {
    let (_root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let pin = registration.server_pin();
    drop(registration);
    let registration = fixture.reopen().discover().unwrap().unwrap();
    let identity = ServerKeyStore::open(&registration)
        .unwrap()
        .load_identity(pin)
        .unwrap();
    let transcript = b"actual protocol transcript";
    UnparsedPublicKey::new(&ED25519, pin.public_key)
        .verify(transcript, &identity.sign(transcript).unwrap())
        .unwrap();
    assert_eq!(identity.pin(), pin);
}

#[test]
fn exclusive_identity_creation_has_one_winner_and_reuses_that_exact_identity() {
    let (_root, fixture) = fixture();
    let first = fixture.identity_scope().unwrap();
    let second = fixture.identity_scope().unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let one = Arc::clone(&barrier);
    let first = std::thread::spawn(move || {
        one.wait();
        first.0.create_identity(first.1)
    });
    let second = std::thread::spawn(move || {
        barrier.wait();
        second.0.create_identity(second.1)
    });
    let results = [first.join().unwrap(), second.join().unwrap()];
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(KeyStoreFailure::AlreadyExists)))
            .count(),
        1
    );
    let winner = results.into_iter().find_map(Result::ok).unwrap().pin();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    assert_eq!(registration.server_pin(), winner);
}

#[test]
fn missing_established_identity_is_not_recreated() {
    let (_root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let store = ServerKeyStore::open(&registration).unwrap();
    fs::remove_file(fixture.data_directory().join("identity")).unwrap();
    assert_eq!(
        store.load_identity(registration.server_pin()).unwrap_err(),
        KeyStoreFailure::Missing
    );
    assert!(!fixture.data_directory().join("identity").exists());
}

#[test]
fn changed_registered_public_pin_cannot_select_another_identity() {
    let (_root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let store = ServerKeyStore::open(&registration).unwrap();
    let mut pin = registration.server_pin();
    pin.public_key[0] ^= 1;
    assert_eq!(
        store.load_identity(pin).unwrap_err(),
        KeyStoreFailure::IdentityMismatch
    );
}

#[test]
fn unsafe_identity_modes_are_rejected_without_repair() {
    let (_root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let store = ServerKeyStore::open(&registration).unwrap();
    let path = fixture.data_directory().join("identity");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    assert_eq!(
        store.load_identity(registration.server_pin()).unwrap_err(),
        KeyStoreFailure::UnsafeStorage
    );
    assert_eq!(fs::metadata(path).unwrap().mode() & 0o777, 0o640);
}

#[test]
fn hardlinked_identity_is_rejected() {
    let (_root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    fs::hard_link(
        fixture.data_directory().join("identity"),
        fixture.data_directory().join("alias"),
    )
    .unwrap();
    assert_eq!(
        ServerKeyStore::open(&registration)
            .unwrap()
            .load_identity(registration.server_pin())
            .unwrap_err(),
        KeyStoreFailure::UnsafeStorage
    );
}

#[test]
fn symlink_identity_is_rejected_without_following_it() {
    let (_root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let identity = fixture.data_directory().join("identity");
    let alternate = fixture.data_directory().join("alternate");
    fs::rename(&identity, &alternate).unwrap();
    symlink(&alternate, &identity).unwrap();
    assert_eq!(
        ServerKeyStore::open(&registration)
            .unwrap()
            .load_identity(registration.server_pin())
            .unwrap_err(),
        KeyStoreFailure::UnsafeStorage
    );
    assert!(alternate.is_file());
}

#[test]
fn replaced_private_directory_invalidates_retained_authority() {
    let (_root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let store = ServerKeyStore::open(&registration).unwrap();
    let moved = fixture.data_directory().with_extension("moved");
    fs::rename(fixture.data_directory(), &moved).unwrap();
    fs::create_dir(fixture.data_directory()).unwrap();
    fs::set_permissions(fixture.data_directory(), fs::Permissions::from_mode(0o700)).unwrap();
    fs::copy(
        moved.join("identity"),
        fixture.data_directory().join("identity"),
    )
    .unwrap();
    assert_eq!(
        store.load_identity(registration.server_pin()).unwrap_err(),
        KeyStoreFailure::UnsafeStorage
    );
}

#[test]
fn unavailable_established_directory_is_not_fresh_installation() {
    let (_root, fixture) = fixture();
    fixture.initialize(DesiredServerState::Stopped).unwrap();
    fs::rename(
        fixture.data_directory(),
        fixture.data_directory().with_extension("moved"),
    )
    .unwrap();
    assert_eq!(
        fixture.reopen().discover().unwrap_err(),
        ServiceFailure::Missing
    );
    assert!(!fixture.data_directory().exists());
}

#[test]
fn pairing_secret_survives_reopening_and_is_removed_by_its_exact_version() {
    let (_root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let store = ServerKeyStore::open(&registration).unwrap();
    let ticket = ticket(registration.server_pin().identity, 1);
    store
        .create_pairing_secret(ticket.clone(), secret())
        .unwrap();
    drop(store);
    let store = ServerKeyStore::open(&fixture.reopen().discover().unwrap().unwrap()).unwrap();
    let (loaded, version) = store.load_pairing_secret(ticket.clone()).unwrap().unwrap();
    assert!(loaded.with_bytes(|bytes| bytes == b"bearer body which must be redacted"));
    store
        .remove_pairing_secret(ticket.clone(), version)
        .unwrap();
    assert!(store.load_pairing_secret(ticket).unwrap().is_none());
}

#[test]
fn secret_creation_is_exclusive() {
    let (_root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let store = ServerKeyStore::open(&registration).unwrap();
    let ticket = ticket(registration.server_pin().identity, 1);
    store
        .create_pairing_secret(ticket.clone(), secret())
        .unwrap();
    assert_eq!(
        store.create_pairing_secret(ticket, secret()).unwrap_err(),
        KeyStoreFailure::AlreadyExists
    );
}

#[test]
fn secret_remove_rejects_a_replacement_even_with_identical_bytes() {
    let (_root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let store = ServerKeyStore::open(&registration).unwrap();
    let ticket = ticket(registration.server_pin().identity, 1);
    let version = store
        .create_pairing_secret(ticket.clone(), secret())
        .unwrap();
    let path = secret_path(&fixture, 1);
    let bytes = fs::read(&path).unwrap();
    fs::rename(&path, fixture.data_directory().join("old-version")).unwrap();
    fs::write(&path, &bytes).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        store.remove_pairing_secret(ticket, version),
        Err(KeyStoreFailure::Conflict)
    );
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn secret_remove_rejects_an_in_place_edit() {
    let (_root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let store = ServerKeyStore::open(&registration).unwrap();
    let ticket = ticket(registration.server_pin().identity, 1);
    let version = store
        .create_pairing_secret(ticket.clone(), secret())
        .unwrap();
    let path = secret_path(&fixture, 1);
    let mut bytes = fs::read(&path).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    fs::write(&path, &bytes).unwrap();
    assert_eq!(
        store.remove_pairing_secret(ticket, version),
        Err(KeyStoreFailure::Conflict)
    );
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn pairing_secret_is_bound_to_the_registered_server() {
    let (_root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let store = ServerKeyStore::open(&registration).unwrap();
    assert_eq!(
        store
            .create_pairing_secret(ticket([1; 16], 1), secret())
            .unwrap_err(),
        KeyStoreFailure::IdentityMismatch
    );
}

#[test]
fn secret_types_and_storage_handles_have_redacted_debug() {
    let (_root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let store = ServerKeyStore::open(&registration).unwrap();
    let identity = store.load_identity(registration.server_pin()).unwrap();
    let ticket = ticket(registration.server_pin().identity, 1);
    let version = store.create_pairing_secret(ticket, secret()).unwrap();
    assert_eq!(
        format!(
            "{:?} {:?} {:?} {:?} {:?}",
            secret(),
            identity,
            version,
            store,
            registration
        ),
        "PairingSecret([redacted]) ServerIdentity([redacted]) SecretVersion(..) ServerKeyStore(..) RegisteredServer(..)"
    );
}

#[test]
fn oversized_and_empty_bearer_secrets_are_refused_before_publication() {
    assert_eq!(
        PairingSecret::new(Zeroizing::new(vec![])).unwrap_err(),
        KeyStoreFailure::InvalidRecord
    );
    assert_eq!(
        PairingSecret::new(Zeroizing::new(vec![1; 4097])).unwrap_err(),
        KeyStoreFailure::LimitExceeded
    );
}

#[test]
fn private_modes_are_exact_under_umask_0002_and_0077() {
    const VARIABLE: &str = "SPACETERM_KEY_MODE_FIXTURE";
    if let Ok(mask) = std::env::var(VARIABLE) {
        let mask = libc::mode_t::from_str_radix(&mask, 8).unwrap();
        // Isolated test executable has no other running tests.
        unsafe { libc::umask(mask) };
        let (_root, fixture) = fixture();
        let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
        let store = ServerKeyStore::open(&registration).unwrap();
        store
            .create_pairing_secret(ticket(registration.server_pin().identity, 1), secret())
            .unwrap();
        assert_eq!(
            fs::metadata(fixture.data_directory()).unwrap().mode() & 0o7777,
            0o700
        );
        for path in [
            fixture.data_directory().join("identity"),
            secret_path(&fixture, 1),
        ] {
            let facts = fs::metadata(path).unwrap();
            assert_eq!(facts.mode() & 0o7777, 0o600);
            assert_eq!(facts.nlink(), 1);
        }
        return;
    }
    for mask in ["0002", "0077"] {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "private_modes_are_exact_under_umask_0002_and_0077",
                "--nocapture",
            ])
            .env(VARIABLE, mask);
        assert!(
            spaceterm_host::process::spawn(&mut command)
                .unwrap()
                .wait()
                .unwrap()
                .success()
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn macos_extended_acl_on_identity_is_rejected_despite_mode_0600() {
    let (_root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let identity = fixture.data_directory().join("identity");
    let status = spaceterm_host::process::spawn(
        std::process::Command::new("/bin/chmod")
            .args(["+a", "everyone allow read"])
            .arg(&identity),
    )
    .unwrap()
    .wait()
    .unwrap();
    assert!(status.success());
    assert_eq!(fs::metadata(&identity).unwrap().mode() & 0o777, 0o600);
    assert_eq!(
        ServerKeyStore::open(&registration)
            .unwrap()
            .load_identity(registration.server_pin())
            .unwrap_err(),
        KeyStoreFailure::UnsafeStorage
    );
}

#[test]
fn malformed_and_unsupported_identity_records_are_bounded_without_reinitialization() {
    let (_root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let store = ServerKeyStore::open(&registration).unwrap();
    let path = fixture.data_directory().join("identity");
    let original = fs::read(&path).unwrap();
    for bytes in [b"malformed".to_vec(), {
        let mut bytes = original.clone();
        bytes[4] = 7;
        bytes
    }] {
        fs::write(&path, &bytes).unwrap();
        assert_eq!(
            store.load_identity(registration.server_pin()).unwrap_err(),
            KeyStoreFailure::InvalidRecord
        );
        assert_eq!(fs::read(&path).unwrap(), bytes);
    }
    fs::write(&path, vec![1; 61]).unwrap();
    assert_eq!(
        store.load_identity(registration.server_pin()).unwrap_err(),
        KeyStoreFailure::LimitExceeded
    );
    assert_eq!(fs::metadata(path).unwrap().len(), 61);
}

#[cfg(target_os = "macos")]
#[test]
fn macos_extended_directory_acl_rejects_writes_before_publication() {
    let (_root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let store = ServerKeyStore::open(&registration).unwrap();
    let status = spaceterm_host::process::spawn(
        std::process::Command::new("/bin/chmod")
            .args(["+a", "everyone allow read"])
            .arg(fixture.data_directory()),
    )
    .unwrap()
    .wait()
    .unwrap();
    assert!(status.success());
    assert_eq!(
        fs::metadata(fixture.data_directory()).unwrap().mode() & 0o777,
        0o700
    );
    let ticket = ticket(registration.server_pin().identity, 1);
    assert_eq!(
        store.create_pairing_secret(ticket, secret()).unwrap_err(),
        KeyStoreFailure::UnsafeStorage
    );
    assert!(!secret_path(&fixture, 1).exists());
}

#[test]
fn create_returns_the_published_version_not_an_identical_successor() {
    let (_root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let store = ServerKeyStore::open(&registration).unwrap();
    let ticket = ticket(registration.server_pin().identity, 1);
    let path = secret_path(&fixture, 1);
    let successor = path.clone();
    spaceterm_host::server_keys::test_support::after_next_publication(move || {
        let bytes = fs::read(&successor).unwrap();
        // Keep the original inode allocated so the replacement cannot reuse it.
        fs::rename(&successor, successor.with_extension("original")).unwrap();
        fs::write(&successor, bytes).unwrap();
        fs::set_permissions(&successor, fs::Permissions::from_mode(0o600)).unwrap();
    });
    let original_version = store
        .create_pairing_secret(ticket.clone(), secret())
        .unwrap();
    let successor_inode = fs::metadata(&path).unwrap().ino();
    assert_eq!(
        store.remove_pairing_secret(ticket, original_version),
        Err(KeyStoreFailure::Conflict),
        "the original publication must not authorize removal of its successor"
    );
    assert_eq!(fs::metadata(path).unwrap().ino(), successor_inode);
}

#[test]
fn secret_removal_sync_failure_is_indeterminate_and_closes_the_store() {
    let (_root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let store = ServerKeyStore::open(&registration).unwrap();
    let ticket = ticket(registration.server_pin().identity, 1);
    let version = store
        .create_pairing_secret(ticket.clone(), secret())
        .unwrap();
    spaceterm_host::server_keys::test_support::fail_next_removal_sync();
    assert_eq!(
        store.remove_pairing_secret(ticket.clone(), version),
        Err(KeyStoreFailure::Indeterminate)
    );
    assert!(!secret_path(&fixture, 1).exists());
    assert_eq!(
        store.load_pairing_secret(ticket).unwrap_err(),
        KeyStoreFailure::Indeterminate
    );
}

#[test]
fn removal_uncertainty_blocks_other_live_handles_and_reconstructed_storage() {
    let (_root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let store = ServerKeyStore::open(&registration).unwrap();
    let other = ServerKeyStore::open(&registration).unwrap();
    let ticket = ticket(registration.server_pin().identity, 1);
    let version = store
        .create_pairing_secret(ticket.clone(), secret())
        .unwrap();
    spaceterm_host::server_keys::test_support::fail_next_removal_sync();
    assert_eq!(
        store.remove_pairing_secret(ticket.clone(), version),
        Err(KeyStoreFailure::Indeterminate)
    );
    assert_eq!(
        other.load_pairing_secret(ticket).unwrap_err(),
        KeyStoreFailure::Indeterminate
    );
    assert_eq!(
        ServerKeyStore::open(&registration).unwrap_err(),
        KeyStoreFailure::Indeterminate
    );
    assert_eq!(
        fixture.reopen().discover().unwrap_err(),
        ServiceFailure::Indeterminate
    );
}

#[test]
fn interrupted_identity_publication_does_not_become_established_on_reopen() {
    let (_root, fixture) = fixture();
    let (store, permit) = fixture.identity_scope().unwrap();
    spaceterm_host::server_keys::test_support::fail_next_publication_durability();
    assert_eq!(
        store.create_identity(permit).unwrap_err(),
        KeyStoreFailure::Indeterminate
    );
    assert!(fixture.data_directory().join("identity").exists());
    drop(store);
    assert_eq!(
        fixture.identity_scope().unwrap_err(),
        ServiceFailure::Indeterminate
    );
}

#[test]
fn uncertain_removal_blocks_a_restarted_process() {
    const ROOT: &str = "SPACETERM_UNCERTAIN_SECRET_FIXTURE";
    if let Some(root) = std::env::var_os(ROOT) {
        let fixture =
            AccountServiceFixture::new(std::path::Path::new(&root), BuildIdentity::Development)
                .unwrap();
        assert_eq!(
            fixture.reopen().discover().unwrap_err(),
            ServiceFailure::Indeterminate
        );
        return;
    }
    let (root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let store = ServerKeyStore::open(&registration).unwrap();
    let ticket = ticket(registration.server_pin().identity, 1);
    let version = store
        .create_pairing_secret(ticket.clone(), secret())
        .unwrap();
    spaceterm_host::server_keys::test_support::fail_next_removal_sync();
    assert_eq!(
        store.remove_pairing_secret(ticket, version),
        Err(KeyStoreFailure::Indeterminate)
    );
    drop(store);
    drop(registration);
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "uncertain_removal_blocks_a_restarted_process",
            "--nocapture",
        ])
        .env(ROOT, root.path());
    let mut child = spaceterm_host::process::spawn(&mut command).unwrap();
    assert!(child.wait().unwrap().success());
    assert!(!secret_path(&fixture, 1).exists());
}

#[test]
fn retained_key_store_revalidates_ancestor_initialization_fences() {
    use sha2::{Digest as _, Sha256};
    use std::io::Write as _;
    let (_root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let store = ServerKeyStore::open(&registration).unwrap();
    let component = fixture.data_directory().file_name().unwrap();
    use std::os::unix::ffi::OsStrExt as _;
    let mut name = String::from(".spaceterm-init-");
    for byte in Sha256::digest(component.as_bytes()) {
        use std::fmt::Write as _;
        write!(name, "{byte:02x}").unwrap();
    }
    let phase = fixture.data_directory().parent().unwrap().join(name);
    // Simulate a different initialization attempt against the same admitted
    // parent. The child's internal mutation checkpoint is still intact.
    let mut bytes = fs::read(&phase).unwrap();
    bytes[8] ^= 0x80;
    fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&phase)
        .unwrap()
        .write_all(&bytes)
        .unwrap();
    assert_eq!(
        store.load_identity(registration.server_pin()).unwrap_err(),
        KeyStoreFailure::Indeterminate
    );
    assert_eq!(
        ServerKeyStore::open(&registration).unwrap_err(),
        KeyStoreFailure::Indeterminate
    );
    assert_eq!(
        fixture.reopen().discover().unwrap_err(),
        ServiceFailure::Indeterminate
    );
}

#[test]
fn explicit_original_secret_removal_reconciliation_confirms_absence_after_reconstruction() {
    let (_root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let store = ServerKeyStore::open(&registration).unwrap();
    let id = ticket(registration.server_pin().identity, 51);
    let version = store.create_pairing_secret(id.clone(), secret()).unwrap();
    spaceterm_host::server_keys::test_support::fail_next_removal_sync();
    assert_eq!(
        store.remove_pairing_secret(id.clone(), version),
        Err(KeyStoreFailure::Indeterminate)
    );
    let reopened = ServerKeyStore::open(&registration).unwrap_err();
    assert_eq!(reopened, KeyStoreFailure::Indeterminate);
    drop(store);
    drop(registration);
    fixture
        .reopen()
        .resolve_pairing_secret_removal(id.clone())
        .unwrap();
    let registration = fixture.reopen().discover().unwrap().unwrap();
    let reopened = ServerKeyStore::open(&registration).unwrap();
    assert!(reopened.load_pairing_secret(id.clone()).unwrap().is_none());
    let new = PairingSecret::new(Zeroizing::new(b"new secret".to_vec())).unwrap();
    reopened.create_pairing_secret(id.clone(), new).unwrap();
    assert_eq!(
        fixture.reopen().resolve_pairing_secret_removal(id.clone()),
        Err(ServiceFailure::RequestConflict)
    );
    assert_eq!(
        reopened
            .load_pairing_secret(id)
            .unwrap()
            .unwrap()
            .0
            .with_bytes(|bytes| bytes.to_vec()),
        b"new secret"
    );
}

#[test]
fn wrong_ticket_cannot_reconcile_another_original_pending_removal() {
    let (_root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let store = ServerKeyStore::open(&registration).unwrap();
    let id = ticket(registration.server_pin().identity, 52);
    let version = store.create_pairing_secret(id.clone(), secret()).unwrap();
    spaceterm_host::server_keys::test_support::fail_next_removal_sync();
    assert_eq!(
        store.remove_pairing_secret(id.clone(), version),
        Err(KeyStoreFailure::Indeterminate)
    );
    let wrong = ticket(registration.server_pin().identity, 53);
    assert_eq!(
        store.resolve_pairing_secret_removal(wrong),
        Err(KeyStoreFailure::Conflict)
    );
    assert!(matches!(
        store.load_pairing_secret(id.clone()),
        Err(KeyStoreFailure::Indeterminate)
    ));
    store.resolve_pairing_secret_removal(id.clone()).unwrap();
    assert!(store.load_pairing_secret(id).unwrap().is_none());
}

#[test]
fn initial_install_reconciles_original_identity_publication_without_replacement_material() {
    for before_effect in [false, true] {
        let (_root, fixture) = fixture();
        let (store, permit) = fixture.identity_scope().unwrap();
        if before_effect {
            spaceterm_host::account_service::test_support::fail_next_publication_before_effect();
        } else {
            spaceterm_host::account_service::test_support::fail_next_mutation_completion_sync();
        }
        assert_eq!(
            store.create_identity(permit).unwrap_err(),
            KeyStoreFailure::Indeterminate
        );
        drop(store);
        let proposal_or_identity = fs::read(fixture.data_directory().join(if before_effect {
            ".account-proposal"
        } else {
            "identity"
        }))
        .unwrap();
        let original = &proposal_or_identity[proposal_or_identity.len() - 60..];
        let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
        assert_eq!(
            fs::read(fixture.data_directory().join("identity")).unwrap(),
            original
        );
        assert_eq!(
            ServerKeyStore::open(&registration)
                .unwrap()
                .load_identity(registration.server_pin())
                .unwrap()
                .pin(),
            registration.server_pin()
        );
    }
}

#[test]
fn original_secret_publication_recovers_exact_ticket_after_provider_reconstruction() {
    for fault in 0..3 {
        let (_root, fixture) = fixture();
        let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
        let pin = registration.server_pin();
        let store = ServerKeyStore::open(&registration).unwrap();
        let id = ticket(pin.identity, 61);
        match fault {
            0 => {
                spaceterm_host::account_service::test_support::fail_next_publication_before_effect()
            }
            1 => {
                spaceterm_host::account_service::test_support::fail_next_mutation_completion_sync()
            }
            _ => spaceterm_host::server_keys::test_support::fail_next_publication_durability(),
        }
        assert_eq!(
            store
                .create_pairing_secret(id.clone(), secret())
                .unwrap_err(),
            KeyStoreFailure::Indeterminate
        );
        drop(store);
        drop(registration);
        assert_eq!(
            fixture.reopen().discover().unwrap_err(),
            ServiceFailure::Indeterminate
        );
        assert!(
            fixture
                .reopen()
                .resolve_pairing_secret_publication(ticket(pin.identity, 62))
                .is_err()
        );
        assert_eq!(
            fixture.reopen().discover().unwrap_err(),
            ServiceFailure::Indeterminate
        );
        fixture
            .reopen()
            .resolve_pairing_secret_publication(id.clone())
            .unwrap();
        let registration = fixture.reopen().discover().unwrap().unwrap();
        let store = ServerKeyStore::open(&registration).unwrap();
        let (body, version) = store.load_pairing_secret(id.clone()).unwrap().unwrap();
        assert_eq!(
            body.with_bytes(|bytes| bytes.to_vec()),
            secret().with_bytes(|bytes| bytes.to_vec())
        );
        assert_eq!(
            fixture
                .reopen()
                .resolve_pairing_secret_publication(id.clone()),
            Err(ServiceFailure::RequestConflict)
        );
        store.remove_pairing_secret(id, version).unwrap();
    }
}

#[test]
fn retained_key_provider_recovers_only_its_original_secret_publication() {
    let (_root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let store = ServerKeyStore::open(&registration).unwrap();
    let id = ticket(registration.server_pin().identity, 63);
    spaceterm_host::server_keys::test_support::fail_next_publication_durability();
    assert_eq!(
        store
            .create_pairing_secret(id.clone(), secret())
            .unwrap_err(),
        KeyStoreFailure::Indeterminate
    );
    assert!(
        store
            .resolve_identity_publication(registration.server_pin())
            .is_err()
    );
    store
        .resolve_pairing_secret_publication(id.clone())
        .unwrap();
    assert!(store.load_pairing_secret(id).unwrap().is_some());
}
