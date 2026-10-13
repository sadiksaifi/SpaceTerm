use super::*;
use spaceterm_client::{
    directories::DirectoryConnectionPort,
    git::rpc::{GitClientFuture, GitConnectionState, GitReadiness},
};
use spaceterm_core::server_directories::*;
use spaceterm_core::{
    authority::{ConnectionGeneration, ServerEpoch},
    identity::{ClientId, PairingId, ServerId},
};
use std::sync::{Arc, Mutex};

struct Connection {
    state: Mutex<GitConnectionState>,
    queries: Mutex<Vec<DirectoryQueryAction>>,
    class: DirectoryValidationClass,
}
impl Connection {
    fn new(class: DirectoryValidationClass) -> Arc<Self> {
        let server = ServerId::new([1; 16]);
        Arc::new(Self {
            state: Mutex::new(GitConnectionState {
                context: DirectoryContext::new(DirectoryContextParts {
                    server,
                    epoch: ServerEpoch::new(7),
                    pairing: PairingId::new(server, 1).unwrap(),
                    client: ClientId::new(server, 1).unwrap(),
                })
                .unwrap(),
                generation: ConnectionGeneration::new(1),
                readiness: GitReadiness::Ready,
            }),
            queries: Mutex::new(Vec::new()),
            class,
        })
    }
}
impl DirectoryConnectionPort for Connection {
    fn state(&self) -> Result<GitConnectionState, ClientDirectoryFailure> {
        Ok(*self.state.lock().unwrap())
    }
    fn query(
        &self,
        query: DirectoryQuery,
    ) -> GitClientFuture<'_, Result<DirectoryQueryResult, ClientDirectoryFailure>> {
        let action = query.parts().action.clone();
        self.queries.lock().unwrap().push(action.clone());
        Box::pin(async move {
            match action {
                DirectoryQueryAction::Home => Ok(DirectoryQueryResult::Home(
                    ServerDirectory::new(query.parts().context.server(), b"/server/home".to_vec())
                        .unwrap(),
                )),
                DirectoryQueryAction::Validate { directory, purpose } => {
                    Ok(DirectoryQueryResult::Validation(DirectoryValidation {
                        directory,
                        purpose,
                        class: self.class,
                    }))
                }
                DirectoryQueryAction::Browse { directory, .. } => {
                    let mut child = directory.bytes().to_vec();
                    if child.last() != Some(&b'/') {
                        child.push(b'/');
                    }
                    child.extend_from_slice(b"child");
                    Ok(DirectoryQueryResult::Page(Box::new(
                        DirectoryPage::new(DirectoryPageParts {
                            context: query.parts().context,
                            directory,
                            observation: std::num::NonZeroU64::new(1).unwrap(),
                            entries: vec![DirectoryEntry {
                                name: DirectoryEntryName::new(b"child".to_vec()).unwrap(),
                                directory: ServerDirectory::new(
                                    query.parts().context.server(),
                                    child,
                                )
                                .unwrap(),
                                selectable: true,
                            }],
                            next: None,
                        })
                        .unwrap(),
                    )))
                }
            }
        })
    }
}

fn source(connection: Arc<Connection>, runtime: &tokio::runtime::Runtime) -> ServerDirectorySource {
    let server = connection.state().unwrap().context.server();
    ServerDirectorySource::new(
        server,
        "Build server",
        on_executor(connection, runtime.handle().clone()),
        DirectoryPurpose::PaneRepair,
    )
    .unwrap()
}

#[test]
fn desktop_server_directory_selection_validates_the_owning_server_without_local_authority() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let connection = Connection::new(DirectoryValidationClass::AvailableDirectory);
    let source = source(connection.clone(), &runtime);
    runtime.block_on(source.discover_home()).unwrap();
    let selection = runtime
        .block_on(source.select(PickerPath::new("~/project/".into()).unwrap()))
        .unwrap();
    let DirectorySelection::Server(directory) = selection else {
        panic!("server values must never become local pins")
    };
    assert_eq!(directory.bytes(), b"/server/home/project/");
    assert!(matches!(
        connection.queries.lock().unwrap().last(),
        Some(DirectoryQueryAction::Validate {
            purpose: DirectoryPurpose::PaneRepair,
            ..
        })
    ));
    assert!(source.is_server_backed());
    assert!(!source.can_create());
    assert!(
        runtime
            .block_on(source.pin(PickerPath::new("/same/path".into()).unwrap()))
            .is_err()
    );
    assert!(
        runtime
            .block_on(
                source.create_directory_recursively(PickerPath::new("/same/path".into()).unwrap())
            )
            .is_err()
    );
    assert_eq!(connection.queries.lock().unwrap().len(), 2);
}

#[test]
fn desktop_server_directory_browsing_reuses_authenticated_queries_and_stays_offline() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let connection = Connection::new(DirectoryValidationClass::AvailableDirectory);
    let source = source(connection.clone(), &runtime);
    let path = PickerPath::new("/server/project/".into()).unwrap();
    let rows = runtime
        .block_on(source.list_directories(path.clone()))
        .unwrap();
    assert_eq!(rows.rows()[0].name(), "child");
    connection.state.lock().unwrap().readiness = GitReadiness::Offline;
    assert_eq!(
        runtime.block_on(source.probe_exact_path(path)).unwrap_err(),
        DirectorySourceError::ConnectionLost
    );
    assert_eq!(connection.queries.lock().unwrap().len(), 1);
}

#[test]
fn desktop_server_directory_selection_does_not_repair_a_missing_target() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let connection = Connection::new(DirectoryValidationClass::Missing);
    let source = source(connection, &runtime);
    let path = PickerPath::new("/server/missing/".into()).unwrap();
    assert_eq!(
        runtime
            .block_on(source.probe_exact_path(path.clone()))
            .unwrap(),
        ExactPathState::Missing
    );
    assert!(matches!(
        runtime.block_on(source.select(path)),
        Err(DirectorySourceError::Missing)
    ));
}
struct NativeDeadline(Arc<Connection>);
impl DirectoryConnectionPort for NativeDeadline {
    fn state(&self) -> Result<GitConnectionState, ClientDirectoryFailure> {
        self.0.state()
    }
    fn query(
        &self,
        query: DirectoryQuery,
    ) -> GitClientFuture<'_, Result<DirectoryQueryResult, ClientDirectoryFailure>> {
        Box::pin(async move {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            self.0.query(query).await
        })
    }
}
#[gpui::test]
async fn desktop_server_directory_queries_keep_native_deadlines_off_the_gui_executor(
    cx: &mut gpui::TestAppContext,
) {
    cx.executor().allow_parking();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let connection = Connection::new(DirectoryValidationClass::AvailableDirectory);
    let source = ServerDirectorySource::new(
        ServerId::new([1; 16]),
        "Build server",
        on_executor(
            Arc::new(NativeDeadline(connection)),
            runtime.handle().clone(),
        ),
        DirectoryPurpose::PaneRepair,
    )
    .unwrap();
    let task = cx
        .foreground_executor()
        .spawn(async move { source.discover_home().await });
    assert_eq!(task.await.unwrap().as_str(), "/server/home");
}

struct PendingRead {
    connection: Arc<Connection>,
    started: async_channel::Sender<()>,
    cancelled: async_channel::Sender<()>,
}

struct ReadCancelled(async_channel::Sender<()>);

impl Drop for ReadCancelled {
    fn drop(&mut self) {
        self.0.try_send(()).unwrap();
    }
}

impl DirectoryConnectionPort for PendingRead {
    fn state(&self) -> Result<GitConnectionState, ClientDirectoryFailure> {
        self.connection.state()
    }

    fn query(
        &self,
        _: DirectoryQuery,
    ) -> GitClientFuture<'_, Result<DirectoryQueryResult, ClientDirectoryFailure>> {
        Box::pin(async move {
            let _cancelled = ReadCancelled(self.cancelled.clone());
            self.started.send(()).await.unwrap();
            std::future::pending().await
        })
    }
}

#[gpui::test]
async fn desktop_server_directory_cancelled_gui_read_aborts_native_worker(
    cx: &mut gpui::TestAppContext,
) {
    cx.executor().allow_parking();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (started, started_rx) = async_channel::bounded(1);
    let (cancelled, cancelled_rx) = async_channel::bounded(1);
    let source = ServerDirectorySource::new(
        ServerId::new([1; 16]),
        "Build server",
        on_executor(
            Arc::new(PendingRead {
                connection: Connection::new(DirectoryValidationClass::AvailableDirectory),
                started,
                cancelled,
            }),
            runtime.handle().clone(),
        ),
        DirectoryPurpose::PaneRepair,
    )
    .unwrap();
    let task = cx
        .foreground_executor()
        .spawn(async move { source.discover_home().await });
    runtime
        .spawn(async move {
            tokio::time::timeout(std::time::Duration::from_secs(5), started_rx.recv())
                .await
                .unwrap()
                .unwrap();
        })
        .await
        .unwrap();
    drop(task);
    runtime
        .spawn(async move {
            tokio::time::timeout(std::time::Duration::from_secs(5), cancelled_rx.recv())
                .await
                .unwrap()
                .unwrap();
        })
        .await
        .unwrap();
}
