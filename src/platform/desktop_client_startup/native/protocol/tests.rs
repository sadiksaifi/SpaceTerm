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

async fn authenticated_case(major: u32, cx: &mut TestAppContext, terminal: bool) {
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
    plan.query_handler = Some(if terminal {
        terminal_publication
    } else {
        publication
    });
    if terminal {
        let seeded = executor.block_on(terminal_fixture::Fixture::new(true));
        TERMINAL_SNAPSHOT.get_or_init(|| seeded.server.snapshot().unwrap());
        plan.capabilities.extend([
            spaceterm_protocol::negotiation::CAPABILITY_ORGANIZATION_COMPARISONS,
            spaceterm_protocol::negotiation::CAPABILITY_TERMINAL_STREAMS,
            spaceterm_protocol::negotiation::CAPABILITY_NATIVE_NOTIFICATIONS,
        ]);
    }
    let plan = Arc::new(plan);
    let startup = NativeDesktopStartup::new(
        NativeDesktopOwners {
            #[cfg(all(feature = "desktop-test-support", debug_assertions))]
            fixture: false,
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
                additional_capabilities: if terminal {
                    desktop_capabilities()
                } else {
                    vec![]
                },
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
    if terminal {
        terminal_acceptance(&protocol, &plan, &executor, info).await;
        let quit =
            cx.update(|cx| installed.update(cx, |runtime, cx| runtime.prepare_client_quit(cx)));
        quit.await.unwrap();
        return;
    }
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
    authenticated_case(2, cx, false).await;
}
#[gpui::test]
async fn desktop_composed_provider_negotiates_previous_major_with_native_ports(
    cx: &mut TestAppContext,
) {
    authenticated_case(1, cx, false).await;
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

#[path = "../../../../../crates/spaceterm-client/tests/projection/support.rs"]
mod terminal_fixture;
static TERMINAL_SNAPSHOT: std::sync::OnceLock<spaceterm_core::organization::OrganizationSnapshot> =
    std::sync::OnceLock::new();

fn terminal_publication(request: &Payload, target: &ConnectionTarget) -> Option<Payload> {
    use spaceterm_core::{content::*, content_queries::*};
    macro_rules! profile {
        ($message:expr, $p:ident, $variant:ident) => {{
            match $message.kind.as_ref()? {
                $p::message::Kind::OrganizationQuery(query) => {
                    let state = TERMINAL_SNAPSHOT.get().unwrap();
                    Some(
                        organization::$p::snapshot_payload(
                            state,
                            0..organization::record_count(state),
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
                $p::message::Kind::ClientContext(context) => {
                    assert!(!context.acknowledged);
                    let mut response = context.clone();
                    response.acknowledged = true;
                    Some(Payload::$variant($p::Message {
                        kind: Some($p::message::Kind::ClientContext(response)),
                    }))
                }
                $p::message::Kind::PaneSubscription(subscription) if !subscription.cancel => {
                    let fence =
                        identity::decode_fence(subscription.fence.as_ref().unwrap(), None).unwrap();
                    let base = ContentBase {
                        session: fence.session(),
                        revision: ContentRevision::new(0),
                        screen: Screen::Primary,
                        screen_generation: 1,
                        topology_revision: 1,
                    };
                    let content_fence = EffectFence::new(
                        fence.session(),
                        fence.grant(),
                        fence.connection(),
                        fence.context(),
                        Some(base.revision),
                    )
                    .unwrap();
                    let stamp = QueryStamp::new(
                        content_fence,
                        base,
                        ContentRequestId::new(subscription.request).unwrap(),
                        RequestGeneration::new(subscription.request_generation),
                    )
                    .unwrap();
                    let limits = subscription.limits.as_ref().unwrap();
                    let request = ContentPageRequest::new(
                        stamp,
                        ContentRange::ActiveGrid,
                        None,
                        PageLimits::new(limits.rows as u16, limits.cells, limits.bytes).unwrap(),
                    )
                    .unwrap();
                    let reply =
                        ContentPageReply::new(&request, terminal_fixture::page(base, 1, false))
                            .unwrap();
                    Some(Payload::$variant($p::Message {
                        kind: Some($p::message::Kind::ContentPublication(Box::new(
                            $p::ContentPublication {
                                request: subscription.request,
                                request_generation: subscription.request_generation,
                                transfer: 1,
                                index: 0,
                                base_revision: 0,
                                response: Some(
                                    spaceterm_protocol::convert::content::$p::encode_page_result(
                                        &ContentReadResult::Ready(reply),
                                    )
                                    .unwrap(),
                                ),
                                complete: true,
                            },
                        ))),
                    }))
                }
                $p::message::Kind::ResourceRequest(value) => {
                    let request =
                        spaceterm_protocol::convert::content::$p::decode_graphics_request(value)
                            .unwrap();
                    Some(Payload::$variant($p::Message {
                        kind: Some($p::message::Kind::ContentReadResponse(Box::new(
                            spaceterm_protocol::convert::content::$p::encode_page_result(
                                &ContentReadResult::Rejected {
                                    request: request.stamp(),
                                    reason: ContentReadFailure::MissingResource,
                                },
                            )
                            .unwrap(),
                        ))),
                    }))
                }
                $p::message::Kind::ContentQuery(query) => {
                    let request =
                        spaceterm_protocol::convert::content::$p::decode_page_request(query)
                            .unwrap();
                    let reply = ContentPageReply::new(
                        &request,
                        terminal_fixture::page(request.stamp().base(), 1, false),
                    )
                    .unwrap();
                    Some(Payload::$variant($p::Message {
                        kind: Some($p::message::Kind::ContentReadResponse(Box::new(
                            spaceterm_protocol::convert::content::$p::encode_page_result(
                                &ContentReadResult::Ready(reply),
                            )
                            .unwrap(),
                        ))),
                    }))
                }
                $p::message::Kind::NativeCompletion(command) => {
                    Some(Payload::$variant($p::Message {
                        kind: Some($p::message::Kind::RuntimeOutcome($p::RuntimeOutcome {
                            request: command.request,
                            fence: command.fence.clone(),
                            ..Default::default()
                        })),
                    }))
                }
                $p::message::Kind::RuntimeCommand(command) => {
                    Some(Payload::$variant($p::Message {
                        kind: Some($p::message::Kind::RuntimeOutcome($p::RuntimeOutcome {
                            request: command.request,
                            fence: command.fence.clone(),
                            ..Default::default()
                        })),
                    }))
                }
                _ => publication(request, target),
            }
        }};
    }
    match request {
        Payload::Major1(message) => profile!(message, v1, Major1),
        Payload::Major2(message) => profile!(message, v2, Major2),
    }
}

fn next_terminal_frame(subscription: &Payload, target: &ConnectionTarget) -> Payload {
    use spaceterm_core::content_queries::*;
    let mut response = terminal_publication(subscription, target).unwrap();
    macro_rules! advance {
        ($message:expr, $p:ident) => {{
            let Some($p::message::Kind::ContentPublication(value)) = $message.kind.as_mut() else {
                panic!("content publication")
            };
            let Some($p::content_read_response::Result::Page(page)) =
                value.response.as_ref().unwrap().result.as_ref()
            else {
                panic!("active grid")
            };
            let old = spaceterm_protocol::convert::content::$p::decode_stamp(
                page.stamp.as_ref().unwrap(),
            )
            .unwrap();
            let mut base = old.base();
            base.revision = ContentRevision::new(1);
            let fence = EffectFence::new(
                old.fence().session(),
                old.fence().grant(),
                old.fence().connection(),
                old.fence().context(),
                Some(base.revision),
            )
            .unwrap();
            let stamp = QueryStamp::new(fence, base, old.request(), old.generation()).unwrap();
            let request = ContentPageRequest::new(
                stamp,
                ContentRange::ActiveGrid,
                None,
                PageLimits::new(256, 16_384, 128 * 1024).unwrap(),
            )
            .unwrap();
            let page =
                ContentPageReply::new(&request, terminal_fixture::page(base, 2, false)).unwrap();
            value.transfer = 2;
            value.response = Some(
                spaceterm_protocol::convert::content::$p::encode_page_result(
                    &ContentReadResult::Ready(page),
                )
                .unwrap(),
            );
        }};
    }
    match &mut response {
        Payload::Major1(message) => advance!(message, v1),
        Payload::Major2(message) => advance!(message, v2),
    }
    response
}

async fn terminal_acceptance(
    protocol: &Arc<NativeProtocol>,
    plan: &Arc<fixture::peer::PeerPlan>,
    executor: &tokio::runtime::Runtime,
    info: ConnectedInfo,
) {
    use spaceterm_client::pane_channel::*;
    use spaceterm_core::content_queries::*;
    for capability in [
        spaceterm_protocol::negotiation::CAPABILITY_ORGANIZATION_COMPARISONS,
        spaceterm_protocol::negotiation::CAPABILITY_TERMINAL_STREAMS,
        spaceterm_protocol::negotiation::CAPABILITY_NATIVE_NOTIFICATIONS,
    ] {
        assert!(
            info.capabilities.supports(capability),
            "installed terminal handler must retain negotiated capability {capability}"
        );
    }
    let snapshot = TERMINAL_SNAPSHOT.get().unwrap();
    let channels = protocol.channels().unwrap();
    let lease = channels
        .attach(
            snapshot.state().workspaces[0].id,
            spaceterm_client::view::AttachmentIncarnation::new(500 + u64::from(info.major))
                .unwrap(),
        )
        .unwrap();
    let pane = lease.pane(snapshot.state().panes[0].id).unwrap();
    pane.update_context(PaneContextFacts {
        focused_pane: true,
        terminal_input_focus: true,
        presentable: true,
    })
    .unwrap();
    executor.block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            while pane.current_frame().is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("production subscription must install the active grid");
        let permit = pane.permit(EffectKind::Read).unwrap();
        let stamp = pane
            .query_stamp(&permit, RequestGeneration::new(1))
            .unwrap();
        let request = ContentPageRequest::new(
            stamp,
            ContentRange::ActiveGrid,
            None,
            PageLimits::new(256, 16384, 128 * 1024).unwrap(),
        )
        .unwrap();
        let mut stream = pane
            .read(&permit, PaneReadRequest::Page(request))
            .await
            .unwrap();
        assert!(matches!(
            stream.next().await.unwrap(),
            Some(PaneReadItem::Page(ContentReadResult::Ready(_)))
        ));
        let subscription = plan
            .messages
            .lock()
            .unwrap()
            .iter()
            .find(|payload| match payload {
                Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::PaneSubscription(value)),
                }) => !value.cancel,
                Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::PaneSubscription(value)),
                }) => !value.cancel,
                _ => false,
            })
            .cloned()
            .unwrap();
        let next = next_terminal_frame(
            &subscription,
            protocol.binding(info.server).unwrap().target(),
        );
        plan.control(fixture::peer::PeerControl::publication(next))
            .await;
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            while pane.current_frame().is_none_or(|frame| {
                frame.frame().unwrap().base().revision != ContentRevision::new(1)
            }) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("later shell output must replace the previous active grid");
        let permit = pane.permit(EffectKind::Read).unwrap();
        let stamp = pane
            .query_stamp(&permit, RequestGeneration::new(3))
            .unwrap();
        let resource = spaceterm_core::content::GraphicsResourceRef {
            key: spaceterm_core::content::GraphicsResourceKey {
                session: stamp.base().session,
                id: spaceterm_core::content::GraphicsResourceId::new(1).unwrap(),
                generation: 1,
            },
            width: 1,
            height: 1,
            rgba_bytes: 4,
            digest: [9; 32],
        };
        let request =
            GraphicsRequest::new(stamp, ResourceTransferId::new(1).unwrap(), resource, 0).unwrap();
        let mut graphics = pane
            .read(&permit, PaneReadRequest::Graphics(request))
            .await
            .unwrap();
        assert!(matches!(
            graphics.next().await.unwrap(),
            Some(PaneReadItem::Graphics(ContentReadResult::Rejected {
                reason: ContentReadFailure::MissingResource,
                ..
            }))
        ));
        let permit = pane.permit(EffectKind::ImeCommit).unwrap();
        pane.try_effect(
            &permit,
            RuntimeEffect::InputMethodCommit(
                spaceterm_terminal::runtime_contracts::BoundedInputMethodText::new("input".into())
                    .unwrap(),
            ),
        )
        .unwrap();
        let permit = pane.permit(EffectKind::Resize).unwrap();
        pane.try_effect(
            &permit,
            RuntimeEffect::Resize(terminal_fixture::grid().into()),
        )
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                let count = plan
                    .messages
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|payload| {
                        matches!(
                            payload,
                            Payload::Major1(v1::Message {
                                kind: Some(v1::message::Kind::RuntimeCommand(_))
                            }) | Payload::Major2(v2::Message {
                                kind: Some(v2::message::Kind::RuntimeCommand(_))
                            })
                        )
                    })
                    .count();
                if count == 2 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let mut events = pane.events().unwrap();
        let permit = pane.permit(EffectKind::NativeCompletion).unwrap();
        let notice = spaceterm_core::native_requests::NativeRequest::new(
            permit.fence(),
            spaceterm_core::native_requests::NativeRequestId::new(1000).unwrap(),
            spaceterm_core::observations::MonotonicTick::new(0),
            spaceterm_core::observations::MonotonicTick::new(0),
            Some(spaceterm_core::observations::MonotonicTick::new(10_000)),
            spaceterm_core::native_requests::NativeRequestKind::Notification(
                spaceterm_core::native_requests::NotificationNotice::Bell,
            ),
        )
        .unwrap();
        let payload = if info.major == 1 {
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::NativeRequest(
                    spaceterm_protocol::convert::native_requests::v1::encode_request(
                        &notice,
                        spaceterm_core::observations::MonotonicTick::new(0),
                    )
                    .unwrap(),
                )),
            })
        } else {
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::NativeRequest(
                    spaceterm_protocol::convert::native_requests::v2::encode_request(
                        &notice,
                        spaceterm_core::observations::MonotonicTick::new(0),
                    )
                    .unwrap(),
                )),
            })
        };
        plan.control(fixture::peer::PeerControl::publication(payload))
            .await;
        let Some(PaneEvent::Native { request, .. }) =
            tokio::time::timeout(std::time::Duration::from_secs(15), events.next())
                .await
                .unwrap()
                .unwrap()
        else {
            panic!("native notification must reach the owned Pane")
        };
        assert_eq!(request.id(), notice.id());
        let completion = spaceterm_core::native_requests::NativeCompletion::new(
            &request,
            spaceterm_core::native_requests::NativeCompletionKind::NotificationAcknowledged,
        )
        .unwrap();
        pane.complete_native(&permit, completion).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                if plan.messages.lock().unwrap().iter().any(|payload| {
                    matches!(
                        payload,
                        Payload::Major1(v1::Message {
                            kind: Some(v1::message::Kind::NativeCompletion(_))
                        }) | Payload::Major2(v2::Message {
                            kind: Some(v2::message::Kind::NativeCompletion(_))
                        })
                    )
                }) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    });
    let frame = pane.current_frame().unwrap();
    assert!(frame.is_paintable());
    assert!(!frame.frame().unwrap().pages().is_empty());
}

#[gpui::test]
async fn desktop_native_terminal_stream_publishes_owned_active_grid(cx: &mut TestAppContext) {
    authenticated_case(2, cx, true).await;
}

#[gpui::test]
async fn desktop_native_terminal_stream_publishes_owned_active_grid_previous_major(
    cx: &mut TestAppContext,
) {
    authenticated_case(1, cx, true).await;
}
