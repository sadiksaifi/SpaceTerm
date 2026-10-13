use super::*;
use spaceterm_client::testing::{FakePaneConnection, FakeServer};
use spaceterm_host::{
    account_service::test_support::AccountServiceFixture, startup::BuildIdentity,
};
use std::{path::PathBuf, sync::Arc};

pub(in crate::platform) struct Root(pub(in crate::platform) PathBuf);
impl Root {
    pub(in crate::platform) fn new() -> Self {
        let mut suffix = [0; 16];
        getrandom::fill(&mut suffix).unwrap();
        #[cfg(target_os = "macos")]
        let parent = PathBuf::from("/private/tmp");
        #[cfg(not(target_os = "macos"))]
        let parent = PathBuf::from("/var/tmp");
        let path = parent.join(format!("st-local-{:x}", u128::from_be_bytes(suffix)));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
pub(super) fn journal() -> (tokio::runtime::Runtime, Arc<JournalController>) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let server = FakeServer::new(12, vec![2, 1]).unwrap();
    let peer = runtime
        .block_on(FakePaneConnection::new(server, 1, 2))
        .unwrap();
    (runtime, peer.journal().clone())
}

#[test]
fn absent_local_registration_does_not_create_server_or_enroll_a_binding() {
    let root = Root::new();
    let fixture = AccountServiceFixture::new(&root.0, BuildIdentity::Development).unwrap();
    let (runtime, journal) = journal();
    let before = journal.snapshot().unwrap();
    let provider = NativeClientProvider::from_supervisor(fixture.reopen());
    assert!(discover(&provider, &journal).unwrap().is_none());
    let target = journal.snapshot().unwrap().bindings()[0].binding().unwrap();
    assert_eq!(
        runtime
            .block_on(trusted_discovery(provider).status(target.target()))
            .unwrap(),
        spaceterm_client::ports::DiscoveryState::Unverified
    );
    assert!(!fixture.anchor().exists());
    assert!(!fixture.data_directory().exists());
    assert_eq!(journal.snapshot().unwrap().revision(), before.revision());
}

#[test]
fn existing_headless_server_keeps_identity_roots_and_client_bindings() {
    let root = Root::new();
    let fixture = AccountServiceFixture::new(&root.0, BuildIdentity::Development).unwrap();
    let original = fixture.initialize(DesiredServerState::Running).unwrap();
    let (_runtime, journal) = journal();
    let before = journal.snapshot().unwrap();
    let found = discover(
        &NativeClientProvider::from_supervisor(fixture.reopen()),
        &journal,
    )
    .unwrap()
    .unwrap();
    assert_eq!(found.registration.server_pin(), original.server_pin());
    assert_eq!(
        found.registration.data_directory(),
        original.data_directory()
    );
    assert_eq!(
        found.registration.organization_directory(),
        original.organization_directory()
    );
    assert_eq!(found.registration.generation(), original.generation());
    assert!(found.binding.is_none());
    assert!(found.route.is_none());
    assert_eq!(journal.snapshot().unwrap().revision(), before.revision());
}

#[test]
fn stopped_registration_is_reused_without_opening_a_missing_endpoint() {
    let root = Root::new();
    let fixture = AccountServiceFixture::new(&root.0, BuildIdentity::Development).unwrap();
    fixture.initialize(DesiredServerState::Stopped).unwrap();
    let (_runtime, journal) = journal();
    let found = discover(
        &NativeClientProvider::from_supervisor(fixture.reopen()),
        &journal,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        found.registration.desired_state(),
        DesiredServerState::Stopped
    );
    assert!(found.route.is_none());
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
fn local_discovery_preserves_release_preflight_and_development_isolation() {
    let root = Root::new();
    let (_runtime, journal) = journal();
    let mut pins = Vec::new();
    for build in [
        BuildIdentity::Release,
        BuildIdentity::Preflight,
        BuildIdentity::Development,
    ] {
        let fixture = AccountServiceFixture::new(&root.0, build).unwrap();
        let original = fixture.initialize(DesiredServerState::Stopped).unwrap();
        let found = discover(
            &NativeClientProvider::from_supervisor(fixture.reopen()),
            &journal,
        )
        .unwrap()
        .unwrap();
        assert_eq!(found.registration.key().build_identity, build);
        assert_eq!(found.registration.server_pin(), original.server_pin());
        assert!(!pins.contains(&original.server_pin()));
        pins.push(original.server_pin());
    }
}

#[test]
fn invalid_existing_registration_is_not_treated_as_missing_or_replaced() {
    use std::os::unix::fs::PermissionsExt;
    let root = Root::new();
    let fixture = AccountServiceFixture::new(&root.0, BuildIdentity::Development).unwrap();
    fixture.initialize(DesiredServerState::Stopped).unwrap();
    let path = fixture.anchor().join("registration");
    std::fs::write(&path, b"invalid retained registration").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (_runtime, journal) = journal();
    let error = match discover(
        &NativeClientProvider::from_supervisor(fixture.reopen()),
        &journal,
    ) {
        Ok(_) => panic!("invalid record admitted"),
        Err(error) => error,
    };
    assert_eq!(error.class(), ClientFailureClass::InvalidStorage);
    assert_eq!(
        std::fs::read(path).unwrap(),
        b"invalid retained registration"
    );
}

#[test]
fn repeated_discovery_never_changes_first_approved_registration_or_stop_intent() {
    let root = Root::new();
    let fixture = AccountServiceFixture::new(&root.0, BuildIdentity::Development).unwrap();
    let original = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let (_runtime, journal) = journal();
    for _ in 0..4 {
        let found = discover(
            &NativeClientProvider::from_supervisor(fixture.reopen()),
            &journal,
        )
        .unwrap()
        .unwrap();
        assert_eq!(found.registration.server_pin(), original.server_pin());
        assert_eq!(found.registration.generation(), original.generation());
        assert_eq!(
            found.registration.desired_state(),
            DesiredServerState::Stopped
        );
    }
}
