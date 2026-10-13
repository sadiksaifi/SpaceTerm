#![cfg(any(target_os = "macos", target_os = "linux"))]

use spaceterm_server::deployment::ServerInvocation;

#[test]
fn deliberate_native_commands_are_distinct_from_read_only_queries_and_serve() {
    for arguments in [
        vec!["owner-enrollment"],
        vec!["reserve-native-request"],
        vec!["setup", "--confirm"],
    ] {
        assert!(ServerInvocation::parse(arguments.into_iter().map(Into::into)).is_ok());
    }
    for arguments in [
        vec!["setup"],
        vec!["--json", "owner-enrollment"],
        vec!["owner-enrollment", "serve"],
        vec!["setup", "--confirm", "--confirm"],
    ] {
        assert!(ServerInvocation::parse(arguments.into_iter().map(Into::into)).is_err());
    }
}

#[cfg(feature = "test-support")]
#[path = "support/account_setup.rs"]
mod setup;

struct RemoteOwnerChannel;
impl spaceterm_host::transport::MessageTransport for RemoteOwnerChannel {
    fn peer_facts(&self) -> spaceterm_host::transport::PeerFacts {
        spaceterm_host::transport::PeerFacts {
            route: spaceterm_host::transport::TransportRoute::SecureWebSocket,
            local_uid: None,
            local_pid: None,
            pinned_server_key: None,
        }
    }
    fn send<'a>(&'a self, _: &'a [u8]) -> spaceterm_host::transport::TransportFuture<'a, ()> {
        Box::pin(async { panic!("remote route must not enter owner framing") })
    }
    fn recv(&self) -> spaceterm_host::transport::TransportFuture<'_, Option<Vec<u8>>> {
        Box::pin(async { panic!("remote route must not enter owner framing") })
    }
    fn close(&self) -> spaceterm_host::transport::TransportFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}
#[tokio::test]
async fn remote_reachability_never_grants_account_owner_enrollment_authority() {
    use spaceterm_host::{
        account_service::test_support::AccountServiceFixture,
        startup::{BuildIdentity, DesiredServerState},
    };
    use spaceterm_server::deployment::owner::{OwnerEndpointFailure, execute};
    let root = tempfile::tempdir().unwrap();
    let fixture = AccountServiceFixture::new(root.path(), BuildIdentity::Development).unwrap();
    let registration = fixture.initialize(DesiredServerState::Stopped).unwrap();
    let supervisor = fixture.reopen();
    supervisor.allocate_request().unwrap();
    let before = std::fs::read(fixture.anchor().join("requests")).unwrap();
    assert_eq!(
        execute(std::sync::Arc::new(supervisor), &RemoteOwnerChannel).await,
        Err(OwnerEndpointFailure::InvalidRequest)
    );
    assert_eq!(
        std::fs::read(fixture.anchor().join("requests")).unwrap(),
        before
    );
    assert!(
        !registration
            .organization_directory()
            .join(spaceterm_host::storage::DATABASE_FILE_NAME)
            .exists()
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
