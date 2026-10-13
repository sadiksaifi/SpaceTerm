//! Real IPC, TLS WebSocket and OpenSSH forwarding share the paired actor path.
//! Production listeners, pairing, SQLite, reducer and native PTYs stay real.
//! Account registration, startup and shutdown authorization are injected. No
//! service is installed and no real account configuration is changed.
#![cfg(any(target_os = "macos", target_os = "linux"))]

#[path = "support/authenticated_routes.rs"]
mod route_fixture;
use route_fixture::*;

async fn exercise(route: Route, major: u32) {
    let fixture = Fixture::new(major).await;
    let mut routes = Routes {
        fixture: &fixture,
        route: Route::Ipc,
        wss: None,
        ssh: None,
        next_endpoint: 0,
    };
    assert!(
        !fixture
            .handle
            .snapshot()
            .state()
            .retained
            .as_ref()
            .unwrap()
            .remote_access()
            .value
            .is_enabled()
    );

    if matches!(route, Route::Wss) {
        // WSS is enabled by an explicit paired, durably identified action. No
        // connection, discovery or SSH action is allowed to synthesize it.
        let mut owner = Client::connect(&mut routes, 0, major).await;
        owner.reconcile(&fixture.handle).await;
        assert!(owner.status().await.parts().listeners.is_empty());
        assert_eq!(
            owner
                .command(Command::SetRemoteAccess {
                    server: ServerId::new(fixture.pin.identity),
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
                        ServerId::new(fixture.pin.identity),
                        vec!["127.0.0.1:0".parse().unwrap()]
                    )
                    .unwrap(),
                })
                .await,
            Outcome::Applied
        );
        owner.route.close().await;
        routes.wss = Some(enabled_status(&mut routes, major).await);
    }
    if matches!(route, Route::Ssh | Route::Jump) {
        routes.ssh = Some(OwnedSsh::new().await);
    }
    routes.route = route;
    let before = fixture.handle.snapshot();
    let unpaired = routes.open().await;
    let unpaired_principal = authentication::Principal {
        server: fixture.pin.identity,
        device: [99; 16],
        pairing: native::local(u64::MAX),
        client: native::local(u64::MAX),
    };
    assert!(
        matches!(
            handshake(&unpaired, fixture.pin, unpaired_principal, major, 99).await,
            Err(ProtocolError::Authentication | ProtocolError::PairingDenied)
        ),
        "reachability must not grant paired authority"
    );
    unpaired.close().await;
    assert_eq!(fixture.handle.snapshot(), before);
    let mut a = Client::connect(&mut routes, 0, major).await;
    let generation = a.reconcile(&fixture.handle).await;
    // The explicit WSS enablement consumed operation 1 on this client's stream.
    if matches!(route, Route::Wss) {
        a.sequence = 2;
    }
    let mut b = Client::connect(&mut routes, 1, major).await;
    b.reconcile(&fixture.handle).await;
    let create = a.request(Command::CreateWorkspace {
        server: a.stream.server(),
        source: WorkspaceCreationSource::ServerHome {
            server: a.stream.server(),
        },
        name: NameDraft::Custom(NameText::new("route A".into()).unwrap()),
    });
    let created_a = a.submit(&create).await;
    let Outcome::CreatedWithControl(created_a_value) = created_a.clone() else {
        panic!("created with control: {created_a:?}")
    };
    let created_b = b
        .command(Command::CreateWorkspace {
            server: b.stream.server(),
            source: WorkspaceCreationSource::ServerHome {
                server: b.stream.server(),
            },
            name: NameDraft::Custom(NameText::new("route B".into()).unwrap()),
        })
        .await;
    let Outcome::CreatedWithControl(created_b_value) = created_b else {
        panic!("created with control")
    };
    let grant_a = created_a_value.control().grant();
    let grant_b = created_b_value.control().grant();
    assert_ne!(grant_a.pane(), grant_b.pane());
    let mut acquired_grants = Vec::new();
    for (client, grant) in [(&mut a, grant_a), (&mut b, grant_b)] {
        assert_eq!(
            client
                .command(Command::ReleaseControl(ReleaseControlRequest {
                    epoch: fixture.handle.snapshot().epoch(),
                    grant
                }))
                .await,
            Outcome::Applied
        );
        let snapshot = fixture.handle.snapshot();
        let runtime = snapshot
            .runtime()
            .iter()
            .find(|r| r.pane == grant.pane())
            .unwrap();
        let acquired = client
            .command(Command::CompareAcquire(CompareAcquireRequest {
                epoch: snapshot.epoch(),
                pane: grant.pane(),
                client: client.stream.client(),
                expected_generation: runtime.control_generation,
            }))
            .await;
        let Outcome::ControlGranted(acquired) = acquired else {
            panic!("exact unclaimed comparison acquires control: {acquired:?}")
        };
        acquired_grants.push(acquired.grant());
    }
    let snapshot = fixture.handle.snapshot();
    let runtime_b = snapshot
        .runtime()
        .iter()
        .find(|r| r.pane == grant_b.pane())
        .unwrap();
    let losing = a
        .command(Command::CompareAcquire(CompareAcquireRequest {
            epoch: snapshot.epoch(),
            pane: grant_b.pane(),
            client: a.stream.client(),
            expected_generation: runtime_b.control_generation,
        }))
        .await;
    assert_eq!(
        losing,
        Outcome::Failed(OperationFailure::StaleAuthority),
        "comparison must not take over peer's Pane"
    );
    assert_eq!(
        fixture.handle.snapshot().runtime(),
        snapshot.runtime(),
        "a losing comparison leaves both grants unchanged"
    );
    let grant_a = acquired_grants[0];
    let grant_b = acquired_grants[1];
    let session_a = a.start(&fixture.handle, grant_a).await;
    let session_b = b.start(&fixture.handle, grant_b).await;
    let fence_a = a.fence(session_a, grant_a);
    let fence_b = b.fence(session_b, grant_b);
    tokio::join!(a.subscribe(fence_a), b.subscribe(fence_b));
    tokio::join!(
        a.line(
            fence_a,
            "route_count=$((route_count + 1)); printf 'route-A:%s\\n' \"$route_count\""
        ),
        b.line(fence_b, "printf 'route-B:1\\n'")
    );
    tokio::join!(a.output("route-A:1"), b.output("route-B:1"));
    assert!(
        !a.text.contains("route-B:1"),
        "client A must not receive client B's content"
    );
    assert!(
        !b.text.contains("route-A:1"),
        "client B must not receive client A's content"
    );
    let original_frame = a.last_frame.clone();
    assert!(
        a.effect(
            a.fence(
                session_b,
                ControlGrant::new(grant_b.pane(), a.stream.client(), grant_b.generation()).unwrap()
            ),
            RuntimeCommand::Key(spaceterm_terminal::KeyInput::text_input("cross-client-key"))
        )
        .await
        .is_some(),
        "peer's fence must not admit input"
    );
    let retained = fixture.handle.snapshot();
    let next_sequence = a.sequence;
    a.route.close().await;
    let mut a = Client::connect(&mut routes, 0, major).await;
    assert_ne!(a.reconcile(&fixture.handle).await, generation);
    a.sequence = next_sequence;
    assert_eq!(fixture.handle.snapshot().state(), retained.state());
    for pane in [grant_a.pane(), grant_b.pane()] {
        let now = fixture.handle.snapshot();
        let current = now
            .runtime()
            .iter()
            .find(|runtime| runtime.pane == pane)
            .unwrap();
        let previous = retained
            .runtime()
            .iter()
            .find(|runtime| runtime.pane == pane)
            .unwrap();
        assert_eq!(
            current.session_generation, previous.session_generation,
            "disconnect preserves Sessions"
        );
        assert_eq!(
            current.control_generation, previous.control_generation,
            "disconnect preserves grants"
        );
        assert_eq!(current.control, previous.control);
    }
    assert_eq!(
        a.submit(&create).await,
        created_a,
        "reconnect retries original identity without creating another Workspace"
    );
    assert_eq!(fixture.handle.snapshot().state(), retained.state());
    a.route.transport.send(&original_frame).await.unwrap();
    assert!(
        tokio::time::timeout(HARNESS_TIMEOUT, a.route.transport.recv())
            .await
            .unwrap()
            .unwrap()
            .is_none(),
        "a signed frame from the previous handshake must close the replacement connection"
    );
    assert_eq!(fixture.handle.snapshot().state(), retained.state());
    a.route.close().await;
    let mut a = Client::connect(&mut routes, 0, major).await;
    a.reconcile(&fixture.handle).await;
    a.sequence = next_sequence;
    a.context(grant_a.pane()).await;
    assert!(
        a.effect(
            fence_a,
            RuntimeCommand::Key(spaceterm_terminal::KeyInput::text_input("stale-input"))
        )
        .await
        .is_some(),
        "a replacement connection cannot reuse the previous input fence"
    );
    let current = a.fence(session_a, grant_a);
    a.subscribe(current).await;
    a.line(current, "printf 'route-A-after:%s\\n' \"$route_count\"")
        .await;
    a.output("route-A-after:1").await;
    assert!(
        !a.text.contains("stale-input"),
        "rejected input is never replayed into the surviving shell"
    );
    assert!(
        !a.text.lines().any(|line| line.trim() == "route-A:2"),
        "accepted input is not replayed after reconnect"
    );
    assert_eq!(
        fixture
            .handle
            .snapshot()
            .runtime()
            .iter()
            .find(|runtime| runtime.pane == grant_a.pane())
            .unwrap()
            .session_generation,
        session_a.generation
    );
    // Every paired route has equal close authority, including closing work
    // controlled by the other client, but only after server assessment.
    for (client, pane) in [(&mut a, grant_b.pane()), (&mut b, grant_a.pane())] {
        let target = CloseTarget::Pane(pane);
        let assessed = client.command(Command::AssessClose { target }).await;
        let Outcome::Assessment(assessment) = assessed else {
            panic!("server close assessment: {assessed:?}")
        };
        assert_eq!(
            client
                .command(Command::CloseConfirmed {
                    target,
                    assessment: assessment.reference()
                })
                .await,
            Outcome::Applied
        );
        assert!(
            fixture
                .handle
                .snapshot()
                .state()
                .panes
                .iter()
                .all(|p| p.id != pane)
        );
    }
    if !matches!(route, Route::Wss) {
        assert!(
            !fixture
                .handle
                .snapshot()
                .state()
                .retained
                .as_ref()
                .unwrap()
                .remote_access()
                .value
                .is_enabled(),
            "IPC and SSH must never provision network listeners"
        );
    }
    if !matches!(route, Route::Wss) {
        assert!(a.status().await.parts().listeners.is_empty());
    }
    a.route.close().await;
    b.route.close().await;
    drop(routes);
    fixture.shutdown().await;
}

macro_rules! route_test {
    ($name:ident, $route:ident, $major:literal) => {
        #[tokio::test]
        async fn $name() {
            exercise(Route::$route, $major).await;
        }
    };
}
route_test!(paired_ipc_major_1_has_equivalent_ordinary_authority, Ipc, 1);
route_test!(paired_ipc_major_2_has_equivalent_ordinary_authority, Ipc, 2);
route_test!(paired_wss_major_1_has_equivalent_ordinary_authority, Wss, 1);
route_test!(paired_wss_major_2_has_equivalent_ordinary_authority, Wss, 2);
route_test!(paired_ssh_major_1_has_equivalent_ordinary_authority, Ssh, 1);
route_test!(paired_ssh_major_2_has_equivalent_ordinary_authority, Ssh, 2);
route_test!(
    paired_jump_host_major_1_has_equivalent_ordinary_authority,
    Jump,
    1
);
route_test!(
    paired_jump_host_major_2_has_equivalent_ordinary_authority,
    Jump,
    2
);
