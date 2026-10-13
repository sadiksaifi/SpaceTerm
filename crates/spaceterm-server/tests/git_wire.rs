use spaceterm_core::{
    management::*,
    operation_ledger::canonical::CanonicalCodec,
    operation_ledger_contracts::*,
    operations::*,
    organization::*,
    worktrees::{queries::*, *},
};
use spaceterm_host::transport::MessageTransport;
use spaceterm_protocol::{
    codec::Payload,
    convert::{git, identity, operations},
    wire::{common, v1, v2},
};
use spaceterm_server::{
    contracts::*,
    git::dispatch::GitOrdinaryDispatch,
    listeners::ConnectionDispatchPort,
    protocol_dispatch::{ProtocolDispatcher, RestrictedDispatchPort},
};
use std::{
    num::NonZeroU64,
    sync::{Arc, atomic::Ordering},
};

#[allow(dead_code)]
#[path = "git_support/mod.rs"]
mod git_support;
use git_support::Fixture;

struct Restricted;
impl RestrictedDispatchPort for Restricted {
    fn dispatch(
        &self,
        _: spaceterm_protocol::authentication::Principal,
        _: common::RestrictedMessage,
    ) -> ServerFuture<'_, Result<common::RestrictedMessage, ServerFailure>> {
        Box::pin(async {
            Err(spaceterm_server::failure::failure(
                ServerFailureClass::Authentication,
            ))
        })
    }
}
async fn exchange(
    codec: &mut spaceterm_protocol::connection::AuthenticatedConnection,
    client: &dyn MessageTransport,
    payload: Payload,
) -> Payload {
    client.send(&codec.encode(&payload).unwrap()).await.unwrap();
    let bytes = client.recv().await.unwrap().unwrap();
    codec.decode(&bytes).unwrap()
}
fn operation(major: u32, request: &CanonicalOperationRequest) -> Payload {
    match major {
        1 => Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::Operation(
                operations::v1::encode_request(request).unwrap(),
            )),
        }),
        _ => Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::Operation(
                operations::v2::encode_request(request).unwrap(),
            )),
        }),
    }
}
fn lookup(major: u32, binding: OperationBinding) -> Payload {
    match major {
        1 => Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::OperationLookupRequest(
                operations::v1::encode_lookup_request(binding),
            )),
        }),
        _ => Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::OperationLookupRequest(
                operations::v2::encode_lookup_request(binding),
            )),
        }),
    }
}
fn lookup_result(payload: Payload, binding: OperationBinding) -> OperationLookup {
    match payload {
        Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::OperationLookupResult(value)),
        }) => operations::v1::decode_lookup_result(&value, binding).unwrap(),
        Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::OperationLookupResult(value)),
        }) => operations::v2::decode_lookup_result(&value, binding).unwrap(),
        _ => panic!("lookup response"),
    }
}

async fn start_dispatch(
    fixture: &Fixture,
    major: u32,
) -> (
    Arc<spaceterm_host::ipc::IpcTransport>,
    spaceterm_protocol::connection::AuthenticatedConnection,
    tokio::task::JoinHandle<Result<(), ServerFailure>>,
    spaceterm_host::ipc::IpcListener,
) {
    let wire = fixture.wire(major).await;
    let (mut peer, client, mut codec, listener) = wire.into_parts();
    let snapshot = fixture.handle.snapshot();
    let status = ServerStatus::new(ServerStatusParts {
        runtime: Default::default(),
        server: fixture.stream.server(),
        epoch: snapshot.epoch(),
        revision: NonZeroU64::new(1).unwrap(),
        running: None,
        approved: None,
        staged: None,
        native_verification: NativeVerification::Unverified,
        capabilities: vec![],
        remote_access: snapshot
            .state()
            .retained
            .as_ref()
            .unwrap()
            .remote_access()
            .clone(),
        listeners: vec![],
        progress: vec![],
    })
    .unwrap();
    let (_status_sender, status_receiver) = tokio::sync::watch::channel(status);
    let dispatcher = ProtocolDispatcher::new(
        fixture.authenticator.clone(),
        status_receiver,
        Arc::new(GitOrdinaryDispatch::new(
            fixture.git.clone(),
            Arc::new(CanonicalCodec),
        )),
        Arc::new(Restricted),
    );
    let handle = fixture.handle.clone();
    let connection = tokio::spawn(async move {
        let result = dispatcher.run(&mut peer, handle).await;
        peer.close().await.unwrap();
        result
    });
    let reconciliation = match major {
        1 => Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::Reconciliation(v1::Reconciliation {
                request: 1,
                server: Some(identity::encode_global(fixture.stream.server().bytes())),
                client: Some(identity::encode_client(fixture.stream.client())),
                epoch: snapshot.epoch().get(),
                organization_revision: snapshot.state().revision.get(),
                ..Default::default()
            })),
        }),
        _ => Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::Reconciliation(v2::Reconciliation {
                request: 1,
                server: Some(identity::encode_global(fixture.stream.server().bytes())),
                client: Some(identity::encode_client(fixture.stream.client())),
                epoch: snapshot.epoch().get(),
                organization_revision: snapshot.state().revision.get(),
                ..Default::default()
            })),
        }),
    };
    exchange(&mut codec, client.as_ref(), reconciliation).await;
    (client, codec, connection, listener)
}

#[tokio::test]
async fn git_queries_and_confirmed_writes_round_trip_both_majors() {
    for major in [1, 2] {
        let fixture = Fixture::new().await;
        let (client, mut codec, connection, listener) = start_dispatch(&fixture, major).await;
        let query = GitQueryRequest::new(
            GitCorrelation::new(2, 1).unwrap(),
            GitQuery::Inventory {
                workspace: fixture.workspace,
                cursor: None,
            },
            None,
            false,
        )
        .unwrap();
        let inventory = exchange(
            &mut codec,
            client.as_ref(),
            git::encode_query(major, &query).unwrap(),
        )
        .await;
        let inventory = git::decode_response(&inventory, fixture.stream.server()).unwrap();
        assert_eq!(inventory.correlation, query.correlation);
        assert!(matches!(inventory.result, Ok(GitQueryResult::Inventory(_))));
        let repository = inventory.scope.unwrap();
        let target = fixture.directory.0.join("wire-created");
        let preview = GitQueryRequest::new(
            GitCorrelation::new(3, 1).unwrap(),
            GitQuery::PreviewCreate {
                request: WorktreeCreateDraft::new(
                    repository,
                    WorktreeBranchChoice::New {
                        name: BranchName::new("wire-created".into()).unwrap(),
                        base: BranchReference::Local(BranchName::new("main".into()).unwrap()),
                    },
                    WorktreeLocationChoice::Explicit(
                        ServerDirectory::new(
                            fixture.stream.server(),
                            target.as_os_str().as_encoded_bytes().to_vec(),
                        )
                        .unwrap(),
                    ),
                )
                .unwrap(),
            },
            Some(repository),
            false,
        )
        .unwrap();
        let result = exchange(
            &mut codec,
            client.as_ref(),
            git::encode_query(major, &preview).unwrap(),
        )
        .await;
        let response = git::decode_response(&result, fixture.stream.server()).unwrap();
        let Ok(GitQueryResult::Assessment(page)) = response.result else {
            panic!("creation preview")
        };
        let request = CanonicalOperationRequest::new(
            fixture.operation(2),
            Command::Worktree(GitCommand::CreateConfirmed {
                assessment: page.token,
            }),
            &CanonicalCodec,
        )
        .unwrap();
        assert_eq!(
            lookup_result(
                exchange(&mut codec, client.as_ref(), operation(major, &request)).await,
                request.binding()
            ),
            OperationLookup::Pending
        );
        let created = fixture.git.wait(request.binding().id).await.unwrap();
        let completed = lookup_result(
            exchange(
                &mut codec,
                client.as_ref(),
                lookup(major, request.binding()),
            )
            .await,
            request.binding(),
        );
        assert!(
            matches!(completed, OperationLookup::Completed { result: Outcome::Git(ref result), publication_revision } if result == &created && created.revision() == Some(publication_revision))
        );
        let GitMutationOutcomeParts::Created { worktree, .. } = created.parts() else {
            panic!("created")
        };
        let removal = GitQueryRequest::new(
            GitCorrelation::new(4, 1).unwrap(),
            GitQuery::AssessRemove {
                worktree: *worktree,
            },
            Some(repository),
            false,
        )
        .unwrap();
        let result = exchange(
            &mut codec,
            client.as_ref(),
            git::encode_query(major, &removal).unwrap(),
        )
        .await;
        let response = git::decode_response(&result, fixture.stream.server()).unwrap();
        let Ok(GitQueryResult::Assessment(page)) = response.result else {
            panic!("removal assessment")
        };
        let request = CanonicalOperationRequest::new(
            fixture.operation(3),
            Command::Worktree(GitCommand::RemoveConfirmed {
                assessment: page.token,
            }),
            &CanonicalCodec,
        )
        .unwrap();
        lookup_result(
            exchange(&mut codec, client.as_ref(), operation(major, &request)).await,
            request.binding(),
        );
        let removed = fixture.git.wait(request.binding().id).await.unwrap();
        assert!(matches!(
            removed.parts(),
            GitMutationOutcomeParts::Removed { .. }
        ));
        assert_eq!(
            lookup_result(
                exchange(
                    &mut codec,
                    client.as_ref(),
                    lookup(major, request.binding())
                )
                .await,
                request.binding()
            ),
            OperationLookup::Completed {
                result: Outcome::Git(removed.clone()),
                publication_revision: removed.revision().unwrap()
            }
        );
        assert!(!target.exists());
        assert_eq!(fixture.runner.writes.load(Ordering::SeqCst), 2);
        client.close().await.unwrap();
        connection.await.unwrap().unwrap();
        drop(client);
        listener.close().unwrap();
        fixture.stop().await;
    }
}

struct ReleaseGate(Arc<git_support::native::StoreGate>);
impl Drop for ReleaseGate {
    fn drop(&mut self) {
        self.0.release();
    }
}
#[tokio::test]
async fn reconnect_resolves_original_pending_operation() {
    for major in [1, 2] {
        let fixture = Fixture::new().await;
        let (client, mut codec, connection, listener) = start_dispatch(&fixture, major).await;
        let repository = fixture.inventory().await;
        let command = fixture.preview(repository, "lost-ack").await;
        let request = CanonicalOperationRequest::new(
            fixture.operation(2),
            Command::Worktree(command),
            &CanonicalCodec,
        )
        .unwrap();
        let gate = git_support::native::StoreGate::new();
        let _release = ReleaseGate(gate.clone());
        let block = gate.clone();
        *fixture.runner.after_write.lock().unwrap() = Some(Box::new(move || block.block()));
        client
            .send(&codec.encode(&operation(major, &request)).unwrap())
            .await
            .unwrap();
        gate.entered().await;
        // Lose every response on the first connection. The native write and its
        // final reserved transaction remain owned by the server.
        client.close().await.unwrap();
        let _ = connection.await.unwrap();
        drop(client);
        listener.close().unwrap();
        let (client, mut codec, connection, listener) = start_dispatch(&fixture, major).await;
        assert_eq!(
            lookup_result(
                exchange(
                    &mut codec,
                    client.as_ref(),
                    lookup(major, request.binding())
                )
                .await,
                request.binding()
            ),
            OperationLookup::Pending
        );
        assert_eq!(fixture.runner.writes.load(Ordering::SeqCst), 1);
        gate.release();
        let outcome = fixture.git.wait(request.binding().id).await.unwrap();
        let expected = OperationLookup::Completed {
            result: Outcome::Git(outcome.clone()),
            publication_revision: outcome.revision().unwrap(),
        };
        assert_eq!(
            lookup_result(
                exchange(
                    &mut codec,
                    client.as_ref(),
                    lookup(major, request.binding())
                )
                .await,
                request.binding()
            ),
            expected
        );
        assert_eq!(
            lookup_result(
                exchange(&mut codec, client.as_ref(), operation(major, &request)).await,
                request.binding()
            ),
            expected
        );
        assert_eq!(fixture.runner.writes.load(Ordering::SeqCst), 1);
        client.close().await.unwrap();
        connection.await.unwrap().unwrap();
        drop(client);
        listener.close().unwrap();
        fixture.stop().await;
    }
}
