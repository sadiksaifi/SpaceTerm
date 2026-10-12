use spaceterm_client::{
    directories::*,
    git::rpc::{GitClientFuture, GitConnectionState, GitReadiness},
};
use spaceterm_core::{
    authority::{ConnectionGeneration, ServerEpoch},
    identity::{ClientId, PairingId, ServerId},
    organization::ServerDirectory,
    server_directories::*,
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
fn state() -> GitConnectionState {
    let server = ServerId::new([1; 16]);
    GitConnectionState {
        context: DirectoryContext::new(DirectoryContextParts {
            server,
            epoch: ServerEpoch::new(7),
            pairing: PairingId::new(server, 1).unwrap(),
            client: ClientId::new(server, 1).unwrap(),
        })
        .unwrap(),
        generation: ConnectionGeneration::new(1),
        readiness: GitReadiness::Ready,
    }
}
struct Connection {
    state: GitConnectionState,
    calls: AtomicUsize,
}
impl DirectoryConnectionPort for Connection {
    fn state(&self) -> Result<GitConnectionState, ClientDirectoryFailure> {
        Ok(self.state)
    }
    fn query(
        &self,
        _: DirectoryQuery,
    ) -> GitClientFuture<'_, Result<DirectoryQueryResult, ClientDirectoryFailure>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let server = self.state.context.server();
        Box::pin(async move {
            Ok(DirectoryQueryResult::Home(
                ServerDirectory::new(server, b"/remote/home".to_vec()).unwrap(),
            ))
        })
    }
}
#[tokio::test]
async fn owning_server_home_remains_data_and_wrong_server_requests_do_not_send() {
    let connection = Arc::new(Connection {
        state: state(),
        calls: AtomicUsize::new(0),
    });
    let client = DirectoryClient::new(state().context.server(), connection.clone()).unwrap();
    assert_eq!(client.home().await.unwrap().bytes(), b"/remote/home");
    let other = ServerDirectory::new(ServerId::new([2; 16]), b"/wrong/server".to_vec()).unwrap();
    assert_eq!(
        client.validate(other, DirectoryPurpose::PaneRepair).await,
        Err(ClientDirectoryFailure::Remote(
            DirectoryFailure::WrongServer
        ))
    );
    assert_eq!(connection.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn directory_reads_are_unavailable_offline() {
    let mut state = state();
    state.readiness = GitReadiness::Offline;
    let connection = Arc::new(Connection {
        state,
        calls: AtomicUsize::new(0),
    });
    let client = DirectoryClient::new(state.context.server(), connection.clone()).unwrap();
    assert_eq!(client.home().await, Err(ClientDirectoryFailure::Offline));
    assert_eq!(connection.calls.load(Ordering::SeqCst), 0);
}
