//! Stable Client control is exercised over production pinned WSS and real SQLite.
#![cfg(any(target_os = "macos", target_os = "linux"))]
#[path = "support/authenticated_routes.rs"]
mod route_fixture;
use route_fixture::*;

async fn control(major: u32) {
    let fixture = Fixture::new(major).await;
    let mut routes = Routes {
        fixture: &fixture,
        route: Route::Ipc,
        wss: None,
        ssh: None,
        next_endpoint: 0,
    };
    let mut a = Client::connect(&mut routes, 0, major).await;
    a.reconcile(&fixture.handle).await;
    assert_eq!(
        a.command(Command::SetRemoteAccess {
            server: a.stream.server(),
            expected_version: fixture
                .handle
                .snapshot()
                .state()
                .retained
                .as_ref()
                .unwrap()
                .remote_access()
                .version,
            value: RemoteAccessPolicy::enabled(
                a.stream.server(),
                vec!["127.0.0.1:0".parse().unwrap()]
            )
            .unwrap(),
        })
        .await,
        Outcome::Applied
    );
    a.route.close().await;
    routes.wss = Some(enabled_status(&mut routes, major).await);
    routes.route = Route::Wss;
    let mut a = Client::connect(&mut routes, 0, major).await;
    a.sequence = 2;
    let first_connection = a.reconcile(&fixture.handle).await;
    let mut b = Client::connect(&mut routes, 1, major).await;
    b.reconcile(&fixture.handle).await;
    let created = a
        .command(Command::CreateWorkspace {
            server: a.stream.server(),
            source: WorkspaceCreationSource::ServerHome {
                server: a.stream.server(),
            },
            name: NameDraft::Custom(NameText::new("shared control".into()).unwrap()),
        })
        .await;
    let Outcome::CreatedWithControl(created) = created else {
        panic!("creation grants control: {created:?}")
    };
    let original = created.control().grant();
    let other = b
        .command(Command::CreateWorkspace {
            server: b.stream.server(),
            source: WorkspaceCreationSource::ServerHome {
                server: b.stream.server(),
            },
            name: NameDraft::Custom(NameText::new("other grant".into()).unwrap()),
        })
        .await;
    let Outcome::CreatedWithControl(other) = other else {
        panic!("peer creation grants control")
    };
    let other = other.control().grant();
    assert_eq!(original.client(), a.stream.client());
    let epoch = fixture.handle.snapshot().epoch();
    assert_eq!(
        a.command(Command::ReleaseControl(ReleaseControlRequest {
            epoch,
            grant: original
        }))
        .await,
        Outcome::Applied
    );
    let expected_generation = fixture
        .handle
        .snapshot()
        .runtime()
        .iter()
        .find(|v| v.pane == original.pane())
        .unwrap()
        .control_generation;
    let requests = [
        a.request(Command::CompareAcquire(CompareAcquireRequest {
            epoch,
            pane: original.pane(),
            client: a.stream.client(),
            expected_generation,
        })),
        b.request(Command::CompareAcquire(CompareAcquireRequest {
            epoch,
            pane: original.pane(),
            client: b.stream.client(),
            expected_generation,
        })),
    ];
    let (a_result, b_result) = tokio::join!(a.submit(&requests[0]), b.submit(&requests[1]));
    let winner = match (&a_result, &b_result) {
        (Outcome::ControlGranted(grant), Outcome::Failed(OperationFailure::StaleAuthority)) => {
            grant.grant()
        }
        (Outcome::Failed(OperationFailure::StaleAuthority), Outcome::ControlGranted(grant)) => {
            grant.grant()
        }
        other => panic!("exactly one compare-acquire succeeds: {other:?}"),
    };
    assert_eq!(
        fixture
            .handle
            .snapshot()
            .runtime()
            .iter()
            .find(|v| v.pane == original.pane())
            .unwrap()
            .control,
        ControlRecord::Held {
            client: winner.client(),
            generation: winner.generation()
        }
    );
    // Use the production client presentation policy with the authoritative
    // post-race snapshot. A stale losing acquisition must show Takeover and
    // withdraw content, rather than turn into an implicit takeover.
    use spaceterm_client::view::*;
    let losing_client = if winner.client() == a.stream.client() {
        &b
    } else {
        &a
    };
    let snapshot = fixture.handle.snapshot();
    let replica = ReplicaObservation::new(
        &snapshot,
        &[],
        ReplicaFacts {
            pairing: losing_client.stream.pairing(),
            client: losing_client.stream.client(),
            connection: Some(ConnectionBinding::new(
                losing_client.stream.client(),
                losing_client.generation.unwrap(),
            )),
            readiness: ReplicaReadiness::Ready,
            status: ServerViewStatus::Connected,
        },
    )
    .unwrap();
    let workspace = snapshot
        .state()
        .workspaces
        .iter()
        .find(|w| {
            snapshot
                .state()
                .tabs
                .iter()
                .any(|t| t.workspace == w.id && t.root_pane == original.pane())
        })
        .unwrap()
        .id;
    let mut view = ClientViewState::new();
    view.attach(workspace, &replica).unwrap();
    let facts = PresentationFacts::observe(
        &view,
        &replica,
        workspace,
        original.pane(),
        &StartupIntents::new(),
        None,
    )
    .unwrap();
    let decision = PresentationState::new().reduce(&facts).unwrap();
    assert_eq!(decision.presentation, PanePresentation::Takeover);
    assert_eq!(decision.controller, Some(winner.client()));
    assert_eq!(decision.content, ContentDisposition::Clear);
    assert_eq!(decision.eligibility, PresentationEligibility::NONE);
    // Explicit Take Over, never the losing compare, replaces the grant.
    let next_client = if winner.client() == a.stream.client() {
        &mut b
    } else {
        &mut a
    };
    let Outcome::ControlGranted(taken) = next_client
        .command(Command::TakeOver(TakeOverRequest {
            epoch,
            pane: original.pane(),
            client: next_client.stream.client(),
        }))
        .await
    else {
        panic!("atomic takeover")
    };
    let taken = taken.grant();
    assert_ne!(taken.client(), winner.client());
    assert!(taken.generation().get() > winner.generation().get());
    // An offline Detach carrying the original grant must not release a newer grant.
    assert_eq!(
        a.command(Command::ReleaseControl(ReleaseControlRequest {
            epoch,
            grant: original
        }))
        .await,
        Outcome::Failed(OperationFailure::StaleAuthority)
    );
    assert_eq!(
        fixture
            .handle
            .snapshot()
            .runtime()
            .iter()
            .find(|v| v.pane == original.pane())
            .unwrap()
            .control,
        ControlRecord::Held {
            client: taken.client(),
            generation: taken.generation()
        }
    );
    let next_a = a.sequence;
    let next_b = b.sequence;
    a.route.close().await;
    b.route.close().await;
    assert_eq!(
        fixture
            .handle
            .snapshot()
            .runtime()
            .iter()
            .find(|v| v.pane == original.pane())
            .unwrap()
            .control,
        ControlRecord::Held {
            client: taken.client(),
            generation: taken.generation()
        }
    );
    let mut a = Client::connect(&mut routes, 0, major).await;
    a.sequence = next_a;
    let connection = a.reconcile(&fixture.handle).await;
    assert_ne!(connection, first_connection);
    let mut b = Client::connect(&mut routes, 1, major).await;
    b.sequence = next_b;
    b.reconcile(&fixture.handle).await;
    assert_eq!(
        fixture
            .handle
            .snapshot()
            .runtime()
            .iter()
            .find(|v| v.pane == original.pane())
            .unwrap()
            .control,
        ControlRecord::Held {
            client: taken.client(),
            generation: taken.generation()
        }
    );
    let owner = if taken.client() == a.stream.client() {
        &mut a
    } else {
        &mut b
    };
    assert_eq!(
        owner
            .command(Command::ReleaseControl(ReleaseControlRequest {
                epoch,
                grant: taken
            }))
            .await,
        Outcome::Applied
    );
    assert_eq!(
        fixture
            .handle
            .snapshot()
            .runtime()
            .iter()
            .find(|v| v.pane == original.pane())
            .unwrap()
            .control,
        ControlRecord::Unclaimed
    );
    assert_eq!(
        fixture
            .handle
            .snapshot()
            .runtime()
            .iter()
            .find(|v| v.pane == other.pane())
            .unwrap()
            .control,
        ControlRecord::Held {
            client: other.client(),
            generation: other.generation()
        }
    );
    a.route.close().await;
    b.route.close().await;
    drop(routes);
    fixture.shutdown().await;
}
#[tokio::test]
async fn major_1_two_clients_compare_take_over_reconnect_and_detach_exactly() {
    control(1).await
}
#[tokio::test]
async fn major_2_two_clients_compare_take_over_reconnect_and_detach_exactly() {
    control(2).await
}
