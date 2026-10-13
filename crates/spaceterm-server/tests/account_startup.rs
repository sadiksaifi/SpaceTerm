#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::{
    fs,
    io::{BufRead as _, Write as _},
    os::unix::fs::{PermissionsExt as _, symlink},
    path::Path,
    process::{Command, Stdio},
};

use ring::signature::{ED25519, UnparsedPublicKey};
use spaceterm_core::{
    identity::ServerId, organization::ServerDirectory, storage::OrganizationStore,
};
use spaceterm_host::{
    account_service::{RegisteredServer, test_support::AccountServiceFixture},
    organization_store::{
        SqliteOrganizationStore, StoreIdentity, StoreLimits,
        bootstrap::{InitializationDisposition, NewServerOrigin, NewStoreSeed},
        migrations, schema,
    },
    server_keys::KeyStoreFailure,
    startup::{BuildIdentity, DesiredServerState},
    storage::{Database, StorageFailure},
};
use spaceterm_server::{StartupFailure, account_startup::AccountStartup};

fn fixture() -> (tempfile::TempDir, AccountServiceFixture) {
    // Linux's /tmp may be tmpfs, which the production SQLite authority refuses.
    #[cfg(target_os = "macos")]
    let parent = std::path::PathBuf::from("/private/tmp");
    #[cfg(target_os = "linux")]
    let parent = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    let root = tempfile::tempdir_in(parent).unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    (root, fixture)
}

fn home(root: &Path, registration: &RegisteredServer) -> ServerDirectory {
    ServerDirectory::new(
        ServerId::new(registration.server_pin().identity),
        root.as_os_str().as_encoded_bytes().to_vec(),
    )
    .unwrap()
}

fn initialize_store(root: &Path, registration: &RegisteredServer) {
    // Account installation establishes the private directory, not the domain store.
    let identity = StoreIdentity {
        server: ServerId::new(registration.server_pin().identity),
        account: registration.key(),
        server_public_key: registration.server_pin().public_key,
    };
    let database = Database::open_for_application(
        registration.organization_directory(),
        migrations(),
        schema::APPLICATION_ID,
    )
    .unwrap();
    let services =
        spaceterm_server::adapters::store::production_services(identity, home(root, registration))
            .unwrap();
    assert!(matches!(
        SqliteOrganizationStore::initialize_new(
            database,
            NewStoreSeed {
                identity,
                origin: NewServerOrigin::LocalHeadless,
            },
            StoreLimits::production(),
            services,
        ),
        InitializationDisposition::Durable { .. }
    ));
}

#[test]
fn valid_account_key_and_store_transfer_to_real_projection_without_a_listener() {
    let (root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Running).unwrap();
    initialize_store(root.path(), &registration);
    let expected_pin = registration.server_pin();
    let endpoint = registration.endpoint().unwrap();
    let expected_home = home(root.path(), &registration);
    let startup = AccountStartup::open(registration).unwrap();
    assert_eq!(startup.registration().server_pin(), expected_pin);
    let credentials = startup.credentials();
    let transcript = b"account-startup-signing-test";
    UnparsedPublicKey::new(&ED25519, expected_pin.public_key)
        .verify(transcript, &credentials.sign(transcript).unwrap())
        .unwrap();
    let mut account = startup.open_store(expected_home).unwrap();
    let recovered = account.store.recover().unwrap();
    assert_eq!(recovered.organization.server.bytes(), expected_pin.identity);
    assert!(recovered.organization.workspaces.is_empty());
    assert!(!endpoint.path().exists());
}

#[test]
fn deliberately_stopped_precedes_key_store_and_endpoint_reads() {
    let (_root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    fs::remove_file(fixture.data_directory().join("identity")).unwrap();
    fs::remove_dir(registration.organization_directory()).unwrap();
    // The missing organization directory and broken key do not authorize repair.
    assert_eq!(
        AccountStartup::open(registration).unwrap_err(),
        StartupFailure::DeliberatelyStopped
    );
    assert!(!fixture.data_directory().join("identity").exists());
    assert!(!fixture.data_directory().join("organization").exists());
}

#[test]
fn missing_key_never_creates_a_replacement_or_organization_store() {
    let (_root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Running).unwrap();
    fs::remove_file(fixture.data_directory().join("identity")).unwrap();
    fs::remove_dir(registration.organization_directory()).unwrap();
    assert_eq!(
        AccountStartup::open(registration).unwrap_err(),
        StartupFailure::Key(KeyStoreFailure::Missing)
    );
    assert!(!fixture.data_directory().join("identity").exists());
    assert!(!fixture.data_directory().join("organization").exists());
}

#[test]
fn account_key_file_requires_exact_private_modes() {
    for mode in [0o400, 0o640, 0o644, 0o660] {
        let (_root, fixture) = fixture();
        let registration = fixture.initialize(DesiredServerState::Running).unwrap();
        fs::set_permissions(
            fixture.data_directory().join("identity"),
            fs::Permissions::from_mode(mode),
        )
        .unwrap();
        assert_eq!(
            AccountStartup::open(registration).unwrap_err(),
            StartupFailure::Key(KeyStoreFailure::UnsafeStorage)
        );
    }
}

#[test]
fn account_data_directory_requires_exact_private_modes() {
    let (_root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Running).unwrap();
    fs::set_permissions(fixture.data_directory(), fs::Permissions::from_mode(0o750)).unwrap();
    assert_eq!(
        AccountStartup::open(registration).unwrap_err(),
        StartupFailure::Key(KeyStoreFailure::UnsafeStorage)
    );
}

#[test]
fn symlinked_key_is_not_followed() {
    let (root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Running).unwrap();
    let key = fixture.data_directory().join("identity");
    let replacement = root.path().join("key");
    fs::rename(&key, &replacement).unwrap();
    symlink(&replacement, &key).unwrap();
    assert_eq!(
        AccountStartup::open(registration).unwrap_err(),
        StartupFailure::Key(KeyStoreFailure::UnsafeStorage)
    );
}

#[test]
fn unavailable_organization_store_never_initializes_or_changes_the_key() {
    let (_root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Running).unwrap();
    let before = fs::read(fixture.data_directory().join("identity")).unwrap();
    fs::remove_dir(registration.organization_directory()).unwrap();
    assert_eq!(
        AccountStartup::open(registration).unwrap_err(),
        StartupFailure::Storage(StorageFailure::Unavailable)
    );
    assert!(!fixture.data_directory().join("organization").exists());
    assert!(fs::read(fixture.data_directory().join("identity")).unwrap() == before);
}

#[test]
fn unsafe_database_mode_is_a_failure_not_initialization_permission() {
    let (root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Running).unwrap();
    initialize_store(root.path(), &registration);
    let database = registration
        .organization_directory()
        .join(spaceterm_host::storage::DATABASE_FILE_NAME);
    let before = fs::read(&database).unwrap();
    fs::set_permissions(&database, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        AccountStartup::open(registration).unwrap_err(),
        StartupFailure::Storage(StorageFailure::UnsafeStorage)
    );
    assert_eq!(fs::read(database).unwrap(), before);
}

#[test]
fn locked_store_refuses_startup_without_initializing_or_binding() {
    const LOCK_FIXTURE: &str = "SPACETERM_TEST_LOCKED_STORE";
    if let Some(database) = std::env::var_os(LOCK_FIXTURE) {
        // This adversary is a separate process so its POSIX locks cannot be released
        // by this test's production authority opening the same database inode.
        let connection = rusqlite::Connection::open(database).unwrap();
        connection.execute_batch("BEGIN EXCLUSIVE").unwrap();
        println!("store-lock-acquired");
        std::io::stdout().flush().unwrap();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
        connection.execute_batch("ROLLBACK").unwrap();
        return;
    }
    let (root, fixture) = fixture();
    let registration = fixture.initialize(DesiredServerState::Running).unwrap();
    initialize_store(root.path(), &registration);
    let path = registration
        .organization_directory()
        .join(spaceterm_host::storage::DATABASE_FILE_NAME);
    let before = fs::read(&path).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "locked_store_refuses_startup_without_initializing_or_binding",
            "--nocapture",
        ])
        .env(LOCK_FIXTURE, path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = spaceterm_host::process::spawn(&mut command).unwrap();
    let mut output = std::io::BufReader::new(child.stdout.take().unwrap());
    loop {
        let mut line = String::new();
        assert_ne!(output.read_line(&mut line).unwrap(), 0);
        if line.trim() == "store-lock-acquired" {
            break;
        }
    }
    let result = AccountStartup::open(registration);
    child.stdin.take().unwrap().write_all(b"release\n").unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(
        result.unwrap_err(),
        StartupFailure::Storage(StorageFailure::Unavailable)
    );
    assert_eq!(
        fs::read(
            fixture
                .data_directory()
                .join("organization")
                .join(spaceterm_host::storage::DATABASE_FILE_NAME)
        )
        .unwrap(),
        before
    );
    // Once the original lock is released, startup reuses the same identity/store.
    let registration = fixture.reopen().discover().unwrap().unwrap();
    assert!(AccountStartup::open(registration).is_ok());
}
