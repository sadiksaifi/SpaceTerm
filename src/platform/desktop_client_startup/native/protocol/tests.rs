use super::*;
use gpui::TestAppContext;
use spaceterm_client::{
    ports::*,
    testing::{FakePaneConnection, FakeServer},
};
use spaceterm_protocol::convert::organization::{self, PublicationPart};

use crate::ui::server_client_runtime::testing::wire as fixture;

fn publication(request: &Payload, target: &ConnectionTarget) -> Option<Payload> {
    macro_rules! profile {
        ($message:expr, $p:ident, $variant:ident) => {{
            match $message.kind.as_ref()? {
                $p::message::Kind::OrganizationQuery(query) => {
                    let state = fixture::snapshot(target.server(), 1, 1);
                    Some(
                        organization::$p::snapshot_payload(
                            &state,
                            0..0,
                            PublicationPart {
                                request: query.request,
                                generation: query.request_generation,
                                transfer: 1,
                                index: 0,
                                total: 1,
                                epoch: state.epoch(),
                                base: OrganizationRevision::new(0),
                                revision: state.state().revision,
                            },
                        )
                        .unwrap(),
                    )
                }
                $p::message::Kind::Operation(value) => {
                    let request = operations::$p::decode_request(
                        value,
                        target.pairing(),
                        target.client(),
                        &spaceterm_core::operation_ledger::canonical::CanonicalCodec,
                    )
                    .unwrap();
                    assert!(matches!(
                        request.command(),
                        Command::SetWorktreeLocation { .. }
                    ));
                    Some(if request.binding().id.sequence().get() == 1 {
                        Payload::$variant($p::Message {
                            kind: Some($p::message::Kind::Outcome(
                                operations::$p::encode_outcome(
                                    request.binding().id,
                                    OrganizationRevision::new(1),
                                    &Outcome::Applied,
                                )
                                .unwrap(),
                            )),
                        })
                    } else {
                        panic!("native composition allocated an unexpected operation")
                    })
                }
                $p::message::Kind::Acknowledgement(value) => {
                    let (stream, through) = operations::$p::decode_acknowledgement(
                        value,
                        target.pairing(),
                        target.client(),
                    )
                    .unwrap();
                    Some(Payload::$variant($p::Message {
                        kind: Some($p::message::Kind::AcknowledgementConfirmation(
                            $p::AcknowledgementConfirmation {
                                stream: Some(identity::encode_stream(stream)),
                                acknowledged_through: through.get(),
                            },
                        )),
                    }))
                }
                _ => None,
            }
        }};
    }
    match request {
        Payload::Major1(message) => profile!(message, v1, Major1),
        Payload::Major2(message) => profile!(message, v2, Major2),
    }
}

async fn authenticated_case(major: u32, cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let executor = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap(),
    );
    let server = FakeServer::new(1, vec![major]).unwrap();
    let peer = executor
        .block_on(FakePaneConnection::new(server.clone(), 1, major))
        .unwrap();
    let journal = peer.journal().clone();
    let tracker = OperationTracker::new(journal.clone());
    let protocol = executor
        .block_on(async { Arc::new(NativeProtocol::new(journal.clone(), tracker.clone())) });
    let channels = Arc::new(
        PaneChannels::new(protocol.clone(), ProjectionBudgets::supported().unwrap()).unwrap(),
    );
    protocol.set_channels(channels.clone()).unwrap();
    let mut plan = fixture::peer::PeerPlan::new(vec![major]);
    plan.query_handler = Some(publication);
    let plan = Arc::new(plan);
    let startup = NativeDesktopStartup::new(
        NativeDesktopOwners {
            services: ClientServices {
                routes: Arc::new(fixture::peer::Peers::one(
                    &server.binding(1).unwrap(),
                    plan.clone(),
                )),
                credentials: Arc::new(fixture::Credentials),
                journal: journal.clone(),
                organization: Arc::new(Decoder),
                delegate: protocol.clone(),
                discovery: None,
                limits: spaceterm_client::limits::ClientLimits::default(),
                additional_capabilities: vec![],
            },
            journal,
            channels,
            operations: protocol.clone(),
            invalidator: protocol.clone(),
            tracker: Some(tracker),
            local_server: Some(server.server()),
            registry_binding: Some(protocol.clone()),
            settings: Default::default(),
        },
        crate::platform::desktop_client_startup::tests::MemoryView {
            bytes: None,
            failure: None,
            writes: Arc::default(),
            saved: Arc::default(),
        },
        executor.clone(),
    );
    let recovered = cx.update(|cx| startup.recover(cx)).await.unwrap();
    assert_eq!(plan.opens.load(Ordering::SeqCst), 0);
    let installed = cx.update(|cx| recovered.install(cx).unwrap());
    let registry = protocol.registry().unwrap();
    let handle = registry.server(server.server()).unwrap();
    let info = executor.block_on(async {
        let mut state = handle.state();
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                if let ConnectionState::Connected { info } = *state.borrow() {
                    break info;
                }
                state.changed().await.unwrap();
            }
        })
        .await
        .unwrap()
    });
    assert_eq!(info.major, major);
    assert_eq!(info.client, server.binding(1).unwrap().target().client());
    assert_eq!(plan.opens.load(Ordering::SeqCst), 1);
    assert_eq!(plan.reconcile_requests.load(Ordering::SeqCst), 1);
    cx.run_until_parked();
    assert!(cx.update(|cx| {
        installed
            .read(cx)
            .frame(server.server())
            .is_some_and(|frame| {
                frame.replica_status == crate::ui::server_desktop_contracts::ReplicaStatus::Ready
            })
    }));
    let journal = protocol.journal.clone();
    let binding = server.binding(1).unwrap();
    let ticket = executor
        .block_on(
            protocol.tracker.submit(
                &binding,
                Command::SetWorktreeLocation {
                    server: server.server(),
                    expected_version: FieldVersion::new(1),
                    template: spaceterm_core::organization::WorktreeLocationTemplate::new(
                        "~/.worktrees/{repository}/{branch}".into(),
                    )
                    .unwrap(),
                },
                None,
                protocol.as_ref(),
            ),
        )
        .unwrap();
    let id = ticket.id();
    assert_eq!(
        executor.block_on(protocol.completed(ticket)).unwrap(),
        Outcome::Applied
    );
    executor.block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                if journal
                    .snapshot()
                    .unwrap()
                    .stream(id.stream())
                    .unwrap()
                    .confirmed_prefix
                    == id.sequence().get()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    });
    let quit = cx.update(|cx| installed.update(cx, |runtime, cx| runtime.prepare_client_quit(cx)));
    quit.await.unwrap();
}

#[gpui::test]
async fn desktop_composed_provider_uses_authenticated_native_client_registry(
    cx: &mut TestAppContext,
) {
    authenticated_case(2, cx).await;
}
#[gpui::test]
async fn desktop_composed_provider_negotiates_previous_major_with_native_ports(
    cx: &mut TestAppContext,
) {
    authenticated_case(1, cx).await;
}

#[test]
fn desktop_native_decoder_installs_settings_deltas_without_losing_retained_membership() {
    use spaceterm_client::replica::{OrganizationResidency, Replica, ReplicaUpdate};
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let server = FakeServer::new(7, vec![2, 1]).unwrap();
    let peer = executor
        .block_on(FakePaneConnection::new(server.clone(), 1, 2))
        .unwrap();
    executor
        .block_on(peer.submit(
            Command::CreateWorkspace {
                server: server.server(),
                source: WorkspaceCreationSource::ServerHome {
                    server: server.server(),
                },
                name: spaceterm_core::organization::NameDraft::Automatic,
            },
            None,
        ))
        .unwrap();
    let previous = server.snapshot().unwrap();
    executor
        .block_on(
            peer.submit(
                Command::SetWorktreeLocation {
                    server: server.server(),
                    expected_version: previous.state().settings.worktree_location.version,
                    template: spaceterm_core::organization::WorktreeLocationTemplate::new(
                        "~/.worktrees-test/{repository}/{branch}".into(),
                    )
                    .unwrap(),
                },
                None,
            ),
        )
        .unwrap();
    let current = server.snapshot().unwrap();
    assert!(current.state().revision > previous.state().revision);
    for major in [1, 2] {
        let mut replica = Replica::new(
            server.server(),
            Arc::new(
                OrganizationResidency::new(spaceterm_client::limits::ClientLimits::default())
                    .unwrap(),
            ),
        );
        let token = replica.begin_resync(previous.epoch()).unwrap();
        let part = PublicationPart {
            request: 1,
            generation: token.get(),
            transfer: 1,
            index: 0,
            total: 1,
            epoch: previous.epoch(),
            base: OrganizationRevision::new(0),
            revision: previous.state().revision,
        };
        let count = organization::record_count(&previous);
        let snapshot = if major == 1 {
            organization::v1::snapshot_payload(&previous, 0..count, part)
        } else {
            organization::v2::snapshot_payload(&previous, 0..count, part)
        }
        .unwrap();
        let OrganizationPublication::Snapshot(snapshot) = Decoder.decode(snapshot).unwrap() else {
            panic!("snapshot publication")
        };
        assert_eq!(
            replica.stage_snapshot(token, *snapshot).unwrap(),
            ReplicaUpdate::Installed
        );
        let part = PublicationPart {
            base: previous.state().revision,
            revision: current.state().revision,
            transfer: 2,
            ..part
        };
        macro_rules! delta {
            ($p:ident, $variant:ident) => {
                Payload::$variant($p::Message {
                    kind: Some($p::message::Kind::OrganizationDelta(
                        $p::OrganizationDelta {
                            server: Some(identity::encode_global(server.server().bytes())),
                            epoch: current.epoch().get(),
                            base_revision: previous.state().revision.get(),
                            revision: current.state().revision.get(),
                            settings: Some(organization::$p::encode_settings(
                                &current.state().settings,
                                current.state().retained.as_ref().unwrap().remote_access(),
                            )),
                            part: Some(organization::$p::encode_part(part)),
                            complete: true,
                            ..Default::default()
                        },
                    )),
                })
            };
        }
        let publication = if major == 1 {
            delta!(v1, Major1)
        } else {
            delta!(v2, Major2)
        };
        let OrganizationPublication::DeltaPart { stamp, delta } =
            Decoder.decode(publication).unwrap()
        else {
            panic!("delta publication")
        };
        assert_eq!(
            replica.stage_delta(stamp, delta).unwrap(),
            ReplicaUpdate::Installed
        );
        let observed = replica.subscribe().borrow().retained().cloned().unwrap();
        assert_eq!(observed.organization, current);
    }
}
