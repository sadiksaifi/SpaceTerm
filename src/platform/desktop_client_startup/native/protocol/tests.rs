use super::*;
use gpui::TestAppContext;
use spaceterm_client::{
    pane_channel::{EffectKind, ExplicitUseSource},
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
            spaceterm_protocol::negotiation::CAPABILITY_RUNTIME_METADATA,
            spaceterm_protocol::negotiation::CAPABILITY_RUNTIME_METADATA_FRAGMENTS,
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
                assert!(
                    state.changed().await.is_ok(),
                    "native worker closed during startup: {:?}; messages {}",
                    *state.borrow(),
                    plan.messages.lock().unwrap().len()
                );
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
        executor.block_on(protocol.completed(ticket, info)).unwrap(),
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

fn terminal_context_response(
    request: &Payload,
    acknowledged: bool,
    failure: Option<i32>,
) -> Option<Payload> {
    macro_rules! profile {
        ($message:expr, $p:ident, $variant:ident) => {{
            let Some($p::message::Kind::ClientContext(context)) = $message.kind.as_ref() else {
                return None;
            };
            let mut response = context.clone();
            response.acknowledged = acknowledged;
            response.failure =
                failure.map(|classification| spaceterm_protocol::wire::common::Failure {
                    classification,
                    retry_after_ms: 0,
                });
            Some(Payload::$variant($p::Message {
                kind: Some($p::message::Kind::ClientContext(response)),
            }))
        }};
    }
    match request {
        Payload::Major1(message) => profile!(message, v1, Major1),
        Payload::Major2(message) => profile!(message, v2, Major2),
    }
}

fn terminal_context_generations(request: &Payload) -> (u64, u64) {
    match request {
        Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::ClientContext(context)),
        }) => (context.connection_generation, context.context_generation),
        Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::ClientContext(context)),
        }) => (context.connection_generation, context.context_generation),
        _ => panic!("context request"),
    }
}

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
                    assert!(context.failure.is_none());
                    let rejected = context.native_effects_eligible && !context.terminal_input_focus;
                    terminal_context_response(
                        request,
                        !rejected,
                        rejected.then_some(
                            spaceterm_protocol::wire::common::FailureClass::Stale as i32,
                        ),
                    )
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
                    if request.resource().key.id.get() == 2 {
                        return Some(Payload::$variant($p::Message {
                            kind: Some($p::message::Kind::ResourceChunk(
                                spaceterm_protocol::convert::content::$p::encode_graphics_chunk(
                                    &GraphicsChunk::new(request, 0, vec![7; 4], false).unwrap(),
                                ),
                            )),
                        }));
                    }
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
                $p::message::Kind::ContentQuery(query) if query.cancel => None,
                $p::message::Kind::ContentQuery(query) if matches!(query.kind, 3..=5) => {
                    use spaceterm_protocol::convert::content::reads::{self, ReadQuery};
                    let kind = match reads::$p::decode_query(query).unwrap() {
                        ReadQuery::Topology(request) => {
                            let row = read_fixture_row(request.stamp());
                            $p::message::Kind::ContentReadResponse(Box::new(
                                reads::$p::encode_topology_result(&ContentReadResult::Ready(
                                    TopologyPage::new(&request, vec![row], None).unwrap(),
                                )),
                            ))
                        }
                        ReadQuery::Copy(request) => $p::message::Kind::CopyEvent(
                            reads::$p::encode_copy_event(&CopyStreamItem::Chunk(
                                CopyChunk::new(
                                    &request,
                                    ResourceTransferId::new(50).unwrap(),
                                    CopyPart::Plain,
                                    0,
                                    4,
                                    b"copy".to_vec(),
                                )
                                .unwrap(),
                            )),
                        ),
                        ReadQuery::Find(request) => {
                            let cell = read_fixture_cell(request.stamp());
                            $p::message::Kind::FindEvent(reads::$p::encode_find_event(
                                &FindStreamItem::Chunk(Box::new(
                                    FindChunk::new(
                                        &request,
                                        0,
                                        vec![FindMatch::new(request.stamp(), cell, cell).unwrap()],
                                        None,
                                    )
                                    .unwrap(),
                                )),
                            ))
                        }
                        ReadQuery::Cancel { .. } => unreachable!(),
                    };
                    Some(Payload::$variant($p::Message { kind: Some(kind) }))
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
        spaceterm_protocol::negotiation::CAPABILITY_RUNTIME_METADATA,
        spaceterm_protocol::negotiation::CAPABILITY_RUNTIME_METADATA_FRAGMENTS,
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
    let pane = executor.block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                let attempt = lease.pane(snapshot.state().panes[0].id).and_then(|pane| {
                    pane.update_context(PaneContextFacts {
                        focused_pane: true,
                        terminal_input_focus: true,
                        presentable: true,
                    })?;
                    Ok(pane)
                });
                match attempt {
                    Ok(pane) => break pane,
                    // Initial personal-view persistence briefly closes the real journal gate.
                    Err(error) if error.class() == ClientFailureClass::StaleAuthority => {
                        tokio::task::yield_now().await;
                    }
                    Err(error) => panic!("Pane attachment failed: {error:?}"),
                }
            }
        })
        .await
        .expect("initial journal persistence must release Pane admission")
    });
    pane.update_context(PaneContextFacts {
        focused_pane: true,
        terminal_input_focus: false,
        presentable: true,
    })
    .unwrap();
    executor
        .block_on(protocol.synchronize_presentation(info))
        .unwrap();
    assert!(
        plan.messages
            .lock()
            .unwrap()
            .iter()
            .any(|payload| match payload {
                Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::ClientContext(value)),
                }) => {
                    value.focused_pane.is_some()
                        && !value.terminal_input_focus
                        && !value.native_effects_eligible
                }
                Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::ClientContext(value)),
                }) => {
                    value.focused_pane.is_some()
                        && !value.terminal_input_focus
                        && !value.native_effects_eligible
                }
                _ => false,
            })
    );
    let context_requests = || {
        plan.messages
            .lock()
            .unwrap()
            .iter()
            .filter(|payload| {
                matches!(
                    payload,
                    Payload::Major1(v1::Message {
                        kind: Some(v1::message::Kind::ClientContext(_))
                    }) | Payload::Major2(v2::Message {
                        kind: Some(v2::message::Kind::ClientContext(_))
                    })
                )
            })
            .count()
    };
    let before_publication = context_requests();
    let binding = protocol.binding(info.server).unwrap();
    ConnectionDelegate::invalidate(
        protocol.as_ref(),
        &binding,
        ConnectionInvalidation::Publication,
    );
    let retained = protocol
        .handle(info.server)
        .unwrap()
        .replica()
        .borrow()
        .retained()
        .cloned()
        .unwrap();
    ConnectionDelegate::publication(protocol.as_ref(), info, &retained).unwrap();
    executor
        .block_on(protocol.synchronize_presentation(info))
        .unwrap();
    assert_eq!(
        context_requests(),
        before_publication,
        "organization publication cannot replay an already acknowledged context generation"
    );
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
        drop(stream);
        content_read_acceptance(&pane, protocol, plan, info).await;
        let subscription = plan
            .messages
            .lock()
            .unwrap()
            .iter()
            .rfind(|payload| match payload {
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
        drop(graphics);
        graphics_read_acceptance(&pane, plan, info).await;
        metadata_acceptance(plan, info, &pane).await;
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
        let Some(PaneEvent::Metadata { snapshot, .. }) =
            tokio::time::timeout(std::time::Duration::from_secs(15), events.next())
                .await
                .unwrap()
                .unwrap()
        else {
            panic!("a replacement event subscriber must receive retained metadata")
        };
        assert_eq!(Some(snapshot), pane.metadata_snapshot());
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
    let acknowledged_context = pane.state().borrow().context;
    spaceterm_client::ports::ConnectionDelegate::invalidate(
        protocol.as_ref(),
        &protocol.binding(info.server).unwrap(),
        spaceterm_client::ports::ConnectionInvalidation::Reconciliation,
    );
    assert!(
        pane.state().borrow().context > acknowledged_context,
        "same-connection resynchronization cannot replay its acknowledged context generation"
    );
    let frame = pane.current_frame().unwrap();
    assert!(frame.is_paintable());
    assert!(!frame.frame().unwrap().pages().is_empty());
}

async fn metadata_acceptance(
    plan: &fixture::peer::PeerPlan,
    info: ConnectedInfo,
    pane: &spaceterm_client::pane_channel::PaneChannel,
) {
    use spaceterm_core::observations::{MonotonicTick, RuntimeObservationSequence};
    use spaceterm_protocol::convert::metadata;
    use spaceterm_terminal::{metadata::*, runtime_contracts::*};
    let permit = pane
        .permit(spaceterm_client::pane_channel::EffectKind::Read)
        .unwrap();
    let original = permit.fence();
    let fence = EffectFence::new(
        original.session(),
        original.grant(),
        original.connection(),
        original.context(),
        None,
    )
    .unwrap();
    let initial = RuntimeMetadataSnapshot::new(RuntimeMetadataParts {
        session: fence.session(),
        sequence: RuntimeObservationSequence::new(1000).unwrap(),
        observed_at: MonotonicTick::new(1),
        title: TitleMetadata {
            value: "\u{10400}".repeat(256).into(),
            provenance: TitleProvenance::TerminalControl,
        },
        origin: RuntimeMetadataOrigin::Local {
            user: None,
            host: None,
        },
        progress: ProgressMetadata::Normal(37),
        freshness: MetadataFreshness::Live,
        prompt_zone: PromptZone::CommandOutput,
        hidden_input: false,
    })
    .unwrap();
    let content = pane.current_frame().unwrap().frame().unwrap().base();
    let bytes = metadata::snapshot_bytes(&initial).unwrap();
    for (index, chunk) in bytes.chunks(127).enumerate() {
        macro_rules! fragment {
            ($p:ident, $variant:ident) => {
                Payload::$variant($p::Message {
                    kind: Some($p::message::Kind::MetadataFragment(Box::new(
                        metadata::$p::encode_fragment(
                            fence,
                            900,
                            1,
                            &initial,
                            (index * 127) as u32,
                            chunk,
                            bytes.len() as u32,
                        )
                        .unwrap(),
                    ))),
                })
            };
        }
        let payload = if info.major == 1 {
            fragment!(v1, Major1)
        } else {
            fragment!(v2, Major2)
        };
        plan.control(fixture::peer::PeerControl::publication(payload))
            .await;
    }
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while pane.metadata_snapshot().as_deref() != Some(&initial) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("native registry must publish the fully reconstructed title");
    let mut parts = initial.parts().clone();
    parts.sequence = RuntimeObservationSequence::new(1001).unwrap();
    parts.observed_at = MonotonicTick::new(30001);
    parts.progress = ProgressMetadata::None;
    let expired = RuntimeMetadataSnapshot::new(parts).unwrap();
    let payload = if info.major == 1 {
        Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::MetadataPublication(Box::new(
                metadata::v1::encode_publication(fence, 900, 1, &expired).unwrap(),
            ))),
        })
    } else {
        Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::MetadataPublication(Box::new(
                metadata::v2::encode_publication(fence, 900, 1, &expired).unwrap(),
            ))),
        })
    };
    plan.control(fixture::peer::PeerControl::publication(payload))
        .await;
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        while pane.metadata_snapshot().as_deref() != Some(&expired) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("progress expiry must reach desktop metadata without terminal text");
    assert_eq!(
        pane.current_frame().unwrap().frame().unwrap().base(),
        content
    );
}

#[gpui::test]
async fn desktop_native_terminal_stream_publishes_owned_active_grid(cx: &mut TestAppContext) {
    authenticated_case(2, cx, true).await;
}

#[gpui::test]
async fn desktop_native_previous_major_terminal_stream_publishes_owned_active_grid(
    cx: &mut TestAppContext,
) {
    authenticated_case(1, cx, true).await;
}

fn read_fixture_row(
    stamp: spaceterm_core::content_queries::QueryStamp,
) -> spaceterm_core::content_queries::TopologyRow {
    spaceterm_core::content_queries::TopologyRow::new(
        spaceterm_core::content::RowId {
            screen_generation: stamp.base().screen_generation,
            screen: stamp.base().screen,
            node_serial: 1,
            page_row: 0,
        },
        1,
        80,
        false,
    )
    .unwrap()
}
fn read_fixture_cell(
    stamp: spaceterm_core::content_queries::QueryStamp,
) -> spaceterm_core::content::CellRef {
    spaceterm_core::content::CellRef {
        session: stamp.base().session,
        row: read_fixture_row(stamp).row(),
        row_revision: 1,
        column: 0,
    }
}
async fn content_read_acceptance(
    pane: &spaceterm_client::pane_channel::PaneChannel,
    protocol: &Arc<NativeProtocol>,
    plan: &Arc<fixture::peer::PeerPlan>,
    info: ConnectedInfo,
) {
    use sha2::{Digest, Sha256};
    use spaceterm_client::pane_channel::*;
    use spaceterm_core::content_queries::*;
    use spaceterm_protocol::convert::content::reads;
    let permit = pane.permit(EffectKind::Read).unwrap();
    let stamp = pane
        .query_stamp(&permit, RequestGeneration::new(2))
        .unwrap();
    let topology = TopologyPageRequest::new(
        stamp,
        None,
        PageLimits::new(256, 16384, 128 * 1024).unwrap(),
    )
    .unwrap();
    let mut rows = pane
        .read(&permit, PaneReadRequest::Topology(topology))
        .await
        .expect("topology must use the authenticated read port");
    let Some(PaneReadItem::Topology(ContentReadResult::Ready(page))) = rows.next().await.unwrap()
    else {
        panic!("topology reply")
    };
    assert_eq!(page.rows(), &[read_fixture_row(stamp)]);
    assert!(rows.next().await.unwrap().is_none());
    drop(rows);
    let stamp = pane
        .query_stamp(&permit, RequestGeneration::new(3))
        .unwrap();
    let cell = read_fixture_cell(stamp);
    let copy = CopyRequest::new(
        stamp,
        ContentSelection::new(stamp, cell, cell, false).unwrap(),
        CopyOptions {
            unwrap_soft_wraps: true,
            trim_trailing_spaces: true,
            include_html: false,
        },
    )
    .unwrap();
    let mut copied = pane
        .read(&permit, PaneReadRequest::Copy(copy))
        .await
        .expect("Copy must use the authenticated read port");
    let Some(PaneReadItem::Copy(CopyStreamItem::Chunk(chunk))) = copied.next().await.unwrap()
    else {
        panic!("Copy chunk")
    };
    assert_eq!(chunk.bytes(), b"copy");
    let complete = CopyStreamItem::Complete(
        CopyCompletion::new(
            &copy,
            chunk.transfer(),
            4,
            Sha256::digest(b"copy").into(),
            None,
        )
        .unwrap(),
    );
    let payload = if info.major == 1 {
        Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::CopyEvent(reads::v1::encode_copy_event(
                &complete,
            ))),
        })
    } else {
        Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::CopyEvent(reads::v2::encode_copy_event(
                &complete,
            ))),
        })
    };
    plan.control(fixture::peer::PeerControl::publication(payload))
        .await;
    assert!(matches!(
        copied.next().await.unwrap(),
        Some(PaneReadItem::Copy(CopyStreamItem::Complete(_)))
    ));
    assert!(copied.next().await.unwrap().is_none());
    drop(copied);
    let stamp = pane
        .query_stamp(&permit, RequestGeneration::new(4))
        .unwrap();
    let find = FindRequest::new(
        stamp,
        BoundedFindQuery::new("literal".into()).unwrap(),
        None,
    )
    .unwrap();
    let mut found = pane
        .read(&permit, PaneReadRequest::Find(find))
        .await
        .expect("Find must use the authenticated read port");
    let Some(PaneReadItem::Find(FindStreamItem::Chunk(chunk))) = found.next().await.unwrap() else {
        panic!("Find chunk")
    };
    assert_eq!(
        chunk.matches(),
        &[FindMatch::new(stamp, read_fixture_cell(stamp), read_fixture_cell(stamp)).unwrap()]
    );
    let complete = FindStreamItem::Complete { stamp, chunks: 1 };
    let payload = if info.major == 1 {
        Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::FindEvent(reads::v1::encode_find_event(
                &complete,
            ))),
        })
    } else {
        Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::FindEvent(reads::v2::encode_find_event(
                &complete,
            ))),
        })
    };
    plan.control(fixture::peer::PeerControl::publication(payload))
        .await;
    assert!(matches!(
        found.next().await.unwrap(),
        Some(PaneReadItem::Find(FindStreamItem::Complete {
            chunks: 1,
            ..
        }))
    ));
    assert!(found.next().await.unwrap().is_none());
    drop(found);
    let stamp = pane
        .query_stamp(&permit, RequestGeneration::new(5))
        .unwrap();
    let find =
        FindRequest::new(stamp, BoundedFindQuery::new("cancel".into()).unwrap(), None).unwrap();
    let mut cancelled = pane
        .read(&permit, PaneReadRequest::Find(find))
        .await
        .unwrap();
    assert!(matches!(
        cancelled.next().await.unwrap(),
        Some(PaneReadItem::Find(FindStreamItem::Chunk(_)))
    ));
    drop(cancelled);
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            if plan
                .messages
                .lock()
                .unwrap()
                .iter()
                .any(|payload| match payload {
                    Payload::Major1(v1::Message {
                        kind: Some(v1::message::Kind::ContentQuery(value)),
                    }) => value.cancel,
                    Payload::Major2(v2::Message {
                        kind: Some(v2::message::Kind::ContentQuery(value)),
                    }) => value.cancel,
                    _ => false,
                })
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("dropping a streaming read must cancel server demand");
    assert!(protocol.connected(info.server).is_ok());
}

fn graphics_fixture_payload(
    major: u32,
    chunk: spaceterm_core::content_queries::GraphicsChunk,
) -> Payload {
    if major == 1 {
        Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::ResourceChunk(
                spaceterm_protocol::convert::content::v1::encode_graphics_chunk(&chunk),
            )),
        })
    } else {
        Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::ResourceChunk(
                spaceterm_protocol::convert::content::v2::encode_graphics_chunk(&chunk),
            )),
        })
    }
}
async fn graphics_read_acceptance(
    pane: &spaceterm_client::pane_channel::PaneChannel,
    plan: &Arc<fixture::peer::PeerPlan>,
    info: ConnectedInfo,
) {
    use sha2::{Digest, Sha256};
    use spaceterm_client::pane_channel::*;
    use spaceterm_core::{content::*, content_queries::*};
    let permit = pane.permit(EffectKind::Read).unwrap();
    let stamp = pane
        .query_stamp(&permit, RequestGeneration::new(6))
        .unwrap();
    let resource = GraphicsResourceRef {
        key: GraphicsResourceKey {
            session: stamp.base().session,
            id: GraphicsResourceId::new(2).unwrap(),
            generation: 1,
        },
        width: 2,
        height: 2,
        rgba_bytes: 16,
        digest: Sha256::digest([7; 16]).into(),
    };
    let request =
        GraphicsRequest::new(stamp, ResourceTransferId::new(2).unwrap(), resource, 0).unwrap();
    let mut stream = pane
        .read(&permit, PaneReadRequest::Graphics(request))
        .await
        .unwrap();
    let Some(PaneReadItem::Graphics(ContentReadResult::Ready(first))) =
        stream.next().await.unwrap()
    else {
        panic!("first graphics chunk")
    };
    assert_eq!(first.offset(), 0);
    let mut bytes = first.bytes().to_vec();
    for (offset, length, complete) in [(4, 4, false), (8, 8, true)] {
        let chunk = GraphicsChunk::new(request, offset, vec![7; length], complete).unwrap();
        plan.control(fixture::peer::PeerControl::publication(
            graphics_fixture_payload(info.major, chunk),
        ))
        .await;
        let Some(PaneReadItem::Graphics(ContentReadResult::Ready(chunk))) =
            stream.next().await.unwrap()
        else {
            panic!("successive graphics chunk")
        };
        assert_eq!(chunk.offset(), offset);
        assert_eq!(chunk.complete(), complete);
        bytes.extend_from_slice(chunk.bytes());
    }
    assert_eq!(bytes, [7; 16]);
    assert!(stream.next().await.unwrap().is_none());
    drop(stream);
    let stamp = pane
        .query_stamp(&permit, RequestGeneration::new(7))
        .unwrap();
    let request =
        GraphicsRequest::new(stamp, ResourceTransferId::new(3).unwrap(), resource, 0).unwrap();
    let mut cancelled = pane
        .read(&permit, PaneReadRequest::Graphics(request))
        .await
        .unwrap();
    assert!(matches!(
        cancelled.next().await.unwrap(),
        Some(PaneReadItem::Graphics(ContentReadResult::Ready(_)))
    ));
    drop(cancelled);
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            if plan
                .messages
                .lock()
                .unwrap()
                .iter()
                .any(|payload| match payload {
                    Payload::Major1(v1::Message {
                        kind: Some(v1::message::Kind::ResourceCancel(value)),
                    }) => value.transfer == 3,
                    Payload::Major2(v2::Message {
                        kind: Some(v2::message::Kind::ResourceCancel(value)),
                    }) => value.transfer == 3,
                    _ => false,
                })
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("dropping a graphics read must cancel its exact transfer");
}

struct NativeContextFixture {
    protocol: Arc<NativeProtocol>,
    plan: Arc<fixture::peer::PeerPlan>,
    runtime: spaceterm_client::registry::ClientRuntime,
    info: ConnectedInfo,
    pane: spaceterm_client::pane_channel::PaneChannel,
    _lease: spaceterm_client::pane_channel::WorkspaceLease,
}
impl NativeContextFixture {
    async fn new(major: u32, handler: fixture::peer::QueryHandler) -> Self {
        let seeded = terminal_fixture::Fixture::new(true).await;
        TERMINAL_SNAPSHOT.get_or_init(|| seeded.server.snapshot().unwrap());
        let server = FakeServer::new(1, vec![major]).unwrap();
        let peer = FakePaneConnection::new(server.clone(), 1, major)
            .await
            .unwrap();
        let journal = peer.journal().clone();
        let tracker = OperationTracker::new(journal.clone());
        let protocol = Arc::new(NativeProtocol::new(journal.clone(), tracker));
        let channels = Arc::new(
            PaneChannels::new(protocol.clone(), ProjectionBudgets::supported().unwrap()).unwrap(),
        );
        journal
            .bind_pane_channels(channels.as_ref().clone())
            .unwrap();
        protocol.set_channels(channels.clone()).unwrap();
        let binding = server.binding(1).unwrap();
        let mut plan = fixture::peer::PeerPlan::new(vec![major]);
        plan.query_handler = Some(handler);
        plan.capabilities.extend([
            spaceterm_protocol::negotiation::CAPABILITY_ORGANIZATION_COMPARISONS,
            spaceterm_protocol::negotiation::CAPABILITY_TERMINAL_STREAMS,
        ]);
        let plan = Arc::new(plan);
        let (registry, runtime) = ClientRegistry::recover(ClientServices {
            routes: Arc::new(fixture::peer::Peers::one(&binding, plan.clone())),
            credentials: Arc::new(fixture::Credentials),
            journal,
            organization: Arc::new(Decoder),
            delegate: protocol.clone(),
            discovery: None,
            limits: spaceterm_client::limits::ClientLimits::default(),
            additional_capabilities: desktop_capabilities(),
        })
        .await
        .unwrap();
        protocol.bind(registry.clone()).unwrap();
        let handle = registry.server(binding.target().server()).unwrap();
        handle.connect().await.unwrap();
        let ConnectionState::Connected { info } = *handle.state().borrow() else {
            panic!("native context fixture must connect")
        };
        let snapshot = TERMINAL_SNAPSHOT.get().unwrap();
        let lease = channels
            .attach(
                snapshot.state().workspaces[0].id,
                spaceterm_client::view::AttachmentIncarnation::new(900).unwrap(),
            )
            .unwrap();
        let pane = lease.pane(snapshot.state().panes[0].id).unwrap();
        pane.update_context(spaceterm_client::pane_channel::PaneContextFacts {
            focused_pane: true,
            terminal_input_focus: false,
            presentable: true,
        })
        .unwrap();
        Self {
            protocol,
            plan,
            runtime,
            info,
            pane,
            _lease: lease,
        }
    }
    fn context_requests(&self) -> Vec<Payload> {
        self.plan
            .messages
            .lock()
            .unwrap()
            .iter()
            .filter(|payload| {
                matches!(
                    payload,
                    Payload::Major1(v1::Message {
                        kind: Some(v1::message::Kind::ClientContext(_)),
                    }) | Payload::Major2(v2::Message {
                        kind: Some(v2::message::Kind::ClientContext(_)),
                    })
                )
            })
            .cloned()
            .collect()
    }
    async fn wait_until_presentable(&self) {
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            while self.pane.permit(EffectKind::Resize).is_err() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("acknowledged presentation must become usable");
    }
    async fn pending_context(&self) -> Payload {
        self.context_after(0).await
    }
    async fn context_after(&self, count: usize) -> Payload {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Some(request) = self.context_requests().get(count) {
                    return request.clone();
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("presentation must send its next context request")
    }
    fn use_request(&self) -> PaneUseRequest {
        let state = *self.pane.state().borrow();
        PaneUseRequest {
            workspace: self._lease.workspace(),
            tab: TERMINAL_SNAPSHOT
                .get()
                .unwrap()
                .state()
                .panes
                .iter()
                .find(|pane| pane.id == self.pane.pane())
                .unwrap()
                .tab,
            runtime: state.runtime.unwrap(),
            info: state.connection.unwrap(),
            context: state.context,
            source: ExplicitUseSource::StartSession,
        }
    }
    fn republish(&self) {
        let replica = self
            .protocol
            .handle(self.info.server)
            .unwrap()
            .replica()
            .borrow()
            .retained()
            .cloned()
            .unwrap();
        self.protocol.publication(self.info, &replica).unwrap();
    }
    async fn retry_context(&self, peer: &mut StrictContextPeer, count: usize) -> Payload {
        let mut using = self.protocol.explicit_use(self.use_request());
        let request = tokio::select! {
            biased;
            result = &mut using => panic!("explicit use completed before acknowledgement: {result:?}"),
            request = self.context_after(count) => request,
        };
        self.plan
            .control(fixture::peer::PeerControl::publication(
                peer.accept(&request),
            ))
            .await;
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(5), using)
                .await
                .expect("strict peer acknowledgement must release explicit use")
                .unwrap(),
            PaneUseReply::ControlReady(_)
        ));
        self.wait_until_presentable().await;
        assert!(matches!(
            self.pane
                .explicit_use(ExplicitUseSource::StartSession)
                .await
                .unwrap(),
            spaceterm_client::pane_channel::ExplicitUseOutcome::ControlReady(_)
        ));
        request
    }
    fn pause_sends(&self) {
        let available = self.plan.client_send_gate.available_permits();
        self.plan
            .client_send_gate
            .try_acquire_many(available.try_into().unwrap())
            .unwrap()
            .forget();
    }
    async fn send_started_after(&self, count: usize) {
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while self.plan.sends_started.load(Ordering::SeqCst) <= count {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("fixture traffic must reach the stalled writer");
    }
    fn subscription_cancellation(&self, request: u64) -> Payload {
        let fence = self.pane.permit(EffectKind::Resize).unwrap().fence();
        macro_rules! profile {
            ($p:ident, $variant:ident) => {
                Payload::$variant($p::Message {
                    kind: Some($p::message::Kind::PaneSubscription($p::PaneSubscription {
                        fence: Some(identity::encode_fence(fence)),
                        request,
                        request_generation: fence.context().get(),
                        cancel: true,
                        limits: None,
                    })),
                })
            };
        }
        match self.info.major {
            1 => profile!(v1, Major1),
            2 => profile!(v2, Major2),
            _ => panic!("unsupported fixture profile"),
        }
    }
    async fn assert_unchanged_publication_does_not_replay(&self) {
        let count = self.context_requests().len();
        let binding = self.protocol.binding(self.info.server).unwrap();
        self.protocol
            .invalidate(&binding, ConnectionInvalidation::Publication);
        self.republish();
        assert!(matches!(
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                self.pane.explicit_use(ExplicitUseSource::StartSession),
            )
            .await
            .expect("unchanged publication must reuse the acknowledged context")
            .unwrap(),
            spaceterm_client::pane_channel::ExplicitUseOutcome::ControlReady(_)
        ));
        assert_eq!(self.context_requests().len(), count);
    }
}

#[derive(Default)]
struct StrictContextPeer {
    connection: Option<u64>,
    high_water: u64,
}
impl StrictContextPeer {
    fn accept(&mut self, request: &Payload) -> Payload {
        let (connection, generation) = terminal_context_generations(request);
        if self.connection != Some(connection) {
            self.connection = Some(connection);
            self.high_water = 0;
        }
        let accepted = generation > self.high_water;
        if accepted {
            self.high_water = generation;
        }
        terminal_context_response(
            request,
            accepted,
            (!accepted).then_some(spaceterm_protocol::wire::common::FailureClass::Stale as i32),
        )
        .unwrap()
    }
}

fn held_context_publication(request: &Payload, target: &ConnectionTarget) -> Option<Payload> {
    if terminal_context_response(request, true, None).is_some() {
        None
    } else {
        terminal_publication(request, target)
    }
}
fn stale_context_publication(request: &Payload, target: &ConnectionTarget) -> Option<Payload> {
    terminal_context_response(
        request,
        false,
        Some(spaceterm_protocol::wire::common::FailureClass::Stale as i32),
    )
    .or_else(|| terminal_publication(request, target))
}
fn busy_context_publication(request: &Payload, target: &ConnectionTarget) -> Option<Payload> {
    terminal_context_response(
        request,
        false,
        Some(spaceterm_protocol::wire::common::FailureClass::Busy as i32),
    )
    .or_else(|| terminal_publication(request, target))
}

struct UnusedContextChannel;
impl ProtocolChannel for UnusedContextChannel {
    fn send<'a>(&'a mut self, _: &'a Payload) -> ClientFuture<'a, ()> {
        panic!("context responses must not send on the receive channel")
    }
    fn receive(&mut self) -> ClientFuture<'_, Payload> {
        panic!("context responses must not read from the receive channel")
    }
}

#[test]
fn desktop_native_context_refusals_preserve_the_authenticated_connection() {
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    executor.block_on(async {
        for major in [1, 2] {
            for (handler, expected) in [
                (
                    stale_context_publication as fixture::peer::QueryHandler,
                    ClientFailureClass::StaleAuthority,
                ),
                (
                    busy_context_publication as fixture::peer::QueryHandler,
                    ClientFailureClass::ResourceLimit,
                ),
            ] {
                let case = NativeContextFixture::new(major, handler).await;
                let result = case
                    .pane
                    .explicit_use(ExplicitUseSource::StartSession)
                    .await;
                assert_eq!(result.err().unwrap().class(), expected);
                assert!(matches!(
                    *case.protocol.handle(case.info.server).unwrap().state().borrow(),
                    ConnectionState::Connected { info } if info.binding == case.info.binding
                ));
                case.runtime.shutdown().await.unwrap();
            }
        }
    });
}

#[test]
fn desktop_native_context_preserves_acknowledgement_across_same_connection_invalidation() {
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    executor.block_on(async {
        for major in [1, 2] {
            let case = NativeContextFixture::new(major, terminal_publication).await;
            assert!(matches!(
                case.pane
                    .explicit_use(ExplicitUseSource::StartSession)
                    .await
                    .unwrap(),
                spaceterm_client::pane_channel::ExplicitUseOutcome::ControlReady(_)
            ));
            let requests = case.context_requests();
            assert!(!requests.is_empty());
            for request in &requests {
                macro_rules! check {
                    ($message:expr, $p:ident) => {
                        if let Some($p::message::Kind::ClientContext(context)) =
                            $message.kind.as_ref()
                        {
                            assert!(!context.terminal_input_focus);
                            assert!(!context.native_effects_eligible);
                        }
                    };
                }
                match request {
                    Payload::Major1(message) => check!(message, v1),
                    Payload::Major2(message) => check!(message, v2),
                }
            }
            let binding = case.protocol.binding(case.info.server).unwrap();
            for reason in [
                ConnectionInvalidation::Publication,
                ConnectionInvalidation::Reconciliation,
            ] {
                case.wait_until_presentable().await;
                let before = case.context_requests();
                let (_, acknowledged) = terminal_context_generations(before.last().unwrap());
                let permit = case.pane.permit(EffectKind::Resize).unwrap();
                case.protocol.invalidate(&binding, reason);
                assert!(!permit.is_current());
                let replica = case
                    .protocol
                    .handle(case.info.server)
                    .unwrap()
                    .replica()
                    .borrow()
                    .retained()
                    .cloned()
                    .unwrap();
                case.protocol.publication(case.info, &replica).unwrap();
                case.pane
                    .explicit_use(ExplicitUseSource::StartSession)
                    .await
                    .unwrap();
                let after = case.context_requests();
                if reason == ConnectionInvalidation::Publication {
                    assert_eq!(after.len(), before.len());
                }
                for request in after.iter().skip(before.len()) {
                    let (connection, context) = terminal_context_generations(request);
                    assert_eq!(connection, case.info.binding.generation.get());
                    assert!(context > acknowledged);
                }
            }
            case.runtime.shutdown().await.unwrap();
        }
    });
}

#[test]
fn desktop_native_context_reacknowledges_after_connection_loss() {
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    executor.block_on(async {
        for major in [1, 2] {
            let case = NativeContextFixture::new(major, terminal_publication).await;
            case.pane
                .explicit_use(ExplicitUseSource::StartSession)
                .await
                .unwrap();
            let count = case.context_requests().len();
            let handle = case.protocol.handle(case.info.server).unwrap();
            let mut states = handle.state();
            case.plan
                .control(fixture::peer::PeerControl::Disconnect)
                .await;
            let reconnected = tokio::time::timeout(std::time::Duration::from_secs(15), async {
                loop {
                    if let ConnectionState::Connected { info } = *states.borrow()
                        && info.binding != case.info.binding
                    {
                        break info;
                    }
                    states.changed().await.unwrap();
                }
            })
            .await
            .expect("connection loss must establish a fresh binding");
            case.pane
                .explicit_use(ExplicitUseSource::StartSession)
                .await
                .unwrap();
            let requests = case.context_requests();
            assert!(requests.len() > count);
            let generation = match requests.last().unwrap() {
                Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::ClientContext(context)),
                }) => context.connection_generation,
                Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::ClientContext(context)),
                }) => context.connection_generation,
                _ => unreachable!(),
            };
            assert_eq!(generation, reconnected.binding.generation.get());
            case.runtime.shutdown().await.unwrap();
        }
    });
}

#[test]
fn desktop_native_context_invalidation_discards_pending_acknowledgements() {
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    executor.block_on(async {
        for major in [1, 2] {
            for reason in [
                ConnectionInvalidation::Publication,
                ConnectionInvalidation::Reconciliation,
                ConnectionInvalidation::ConnectionLost,
                ConnectionInvalidation::Stopped,
                ConnectionInvalidation::Shutdown,
                ConnectionInvalidation::BindingRemoved,
            ] {
                let case = NativeContextFixture::new(major, held_context_publication).await;
                let request = case.pending_context().await;
                let binding = case.protocol.binding(case.info.server).unwrap();
                case.protocol.invalidate(&binding, reason);
                let response = terminal_context_response(&request, true, None).unwrap();
                let result = case
                    .protocol
                    .receive(case.info, response, &mut UnusedContextChannel)
                    .await;
                result.unwrap();
                assert!(case.pane.state().borrow().connection.is_none());
                assert!(
                    case.pane
                        .explicit_use(ExplicitUseSource::StartSession)
                        .await
                        .is_err()
                );
                case.runtime.shutdown().await.unwrap();
            }
        }
    });
}

async fn retry_uncertain_context_after_invalidation(reason: ConnectionInvalidation) {
    for major in [1, 2] {
        let case = NativeContextFixture::new(major, held_context_publication).await;
        let mut peer = StrictContextPeer::default();
        let mut using = case.protocol.explicit_use(case.use_request());
        let request = tokio::select! {
            biased;
            result = &mut using => panic!("explicit use completed before acknowledgement: {result:?}"),
            request = case.pending_context() => request,
        };
        let discarded = peer.accept(&request);
        let (_, accepted) = terminal_context_generations(&request);
        let count = case.context_requests().len();
        let binding = case.protocol.binding(case.info.server).unwrap();
        case.protocol.invalidate(&binding, reason);
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), using)
                .await
                .expect("invalidation must promptly release the context owner")
                .unwrap_err()
                .class(),
            ClientFailureClass::StaleAuthority
        );
        case.protocol
            .receive(case.info, discarded, &mut UnusedContextChannel)
            .await
            .unwrap();
        case.republish();
        let retry = case.retry_context(&mut peer, count).await;
        let (connection, retried) = terminal_context_generations(&retry);
        assert_eq!(connection, case.info.binding.generation.get());
        assert!(retried > accepted);
        case.assert_unchanged_publication_does_not_replay().await;
        case.runtime.shutdown().await.unwrap();
    }
}

#[test]
fn desktop_native_context_publication_retries_above_discarded_acknowledgement() {
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    executor.block_on(retry_uncertain_context_after_invalidation(
        ConnectionInvalidation::Publication,
    ));
}

#[test]
fn desktop_native_context_reconciliation_retries_above_discarded_acknowledgement() {
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    executor.block_on(retry_uncertain_context_after_invalidation(
        ConnectionInvalidation::Reconciliation,
    ));
}

#[test]
fn desktop_native_context_timeout_reserves_generation_without_invalidation() {
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    executor.block_on(async {
        for major in [1, 2] {
            let case = NativeContextFixture::new(major, held_context_publication).await;
            let mut peer = StrictContextPeer::default();
            let mut using = case.protocol.explicit_use(case.use_request());
            let request = tokio::select! {
                biased;
                result = &mut using => panic!("explicit use completed before acknowledgement: {result:?}"),
                request = case.pending_context() => request,
            };
            let discarded = peer.accept(&request);
            let (_, accepted) = terminal_context_generations(&request);
            let count = case.context_requests().len();
            assert_eq!(
                tokio::time::timeout(std::time::Duration::from_secs(17), using)
                    .await
                    .expect("unacknowledged context must time out")
                    .unwrap_err()
                    .class(),
                ClientFailureClass::Unavailable
            );
            assert!(matches!(
                *case.protocol.handle(case.info.server).unwrap().state().borrow(),
                ConnectionState::Connected { info } if info.binding == case.info.binding
            ));
            case.protocol
                .receive(case.info, discarded, &mut UnusedContextChannel)
                .await
                .unwrap();
            let retry = case.retry_context(&mut peer, count).await;
            assert!(terminal_context_generations(&retry).1 > accepted);
            case.assert_unchanged_publication_does_not_replay().await;
            case.runtime.shutdown().await.unwrap();
        }
    });
}

#[test]
fn desktop_native_context_dropped_waiter_discards_acknowledgement_and_reserves_generation() {
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    executor.block_on(async {
        for major in [1, 2] {
            let case = NativeContextFixture::new(major, held_context_publication).await;
            let mut peer = StrictContextPeer::default();
            let mut using = case.protocol.explicit_use(case.use_request());
            let request = tokio::select! {
                biased;
                result = &mut using => panic!("explicit use completed before acknowledgement: {result:?}"),
                request = case.pending_context() => request,
            };
            let discarded = peer.accept(&request);
            let (_, accepted) = terminal_context_generations(&request);
            let count = case.context_requests().len();
            drop(using);
            case.protocol
                .receive(case.info, discarded, &mut UnusedContextChannel)
                .await
                .unwrap();
            let retry = case.retry_context(&mut peer, count).await;
            assert!(terminal_context_generations(&retry).1 > accepted);
            case.assert_unchanged_publication_does_not_replay().await;
            case.runtime.shutdown().await.unwrap();
        }
    });
}

#[test]
fn desktop_native_context_clears_focus_after_uncertain_first_presentation_disappears() {
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    executor.block_on(async {
        for major in [1, 2] {
            let case = NativeContextFixture::new(major, held_context_publication).await;
            let mut peer = StrictContextPeer::default();
            let captured = case.use_request();
            let mut using = case.protocol.explicit_use(captured);
            let request = tokio::select! {
                biased;
                result = &mut using => panic!("explicit use completed before acknowledgement: {result:?}"),
                request = case.pending_context() => request,
            };
            let discarded = peer.accept(&request);
            let (_, accepted) = terminal_context_generations(&request);
            let count = case.context_requests().len();
            drop(using);
            case._lease.invalidate().unwrap();
            case.protocol
                .receive(case.info, discarded, &mut UnusedContextChannel)
                .await
                .unwrap();
            let cleared = case.context_after(count).await;
            assert!(terminal_context_generations(&cleared).1 > accepted);
            macro_rules! check {
                ($message:expr, $p:ident) => {
                    if let Some($p::message::Kind::ClientContext(context)) = $message.kind.as_ref() {
                        assert!(context.focused_pane.is_none());
                        assert!(!context.terminal_input_focus);
                        assert!(!context.native_effects_eligible);
                    } else {
                        panic!("focus clearing must send a context request");
                    }
                };
            }
            match &cleared {
                Payload::Major1(message) => check!(message, v1),
                Payload::Major2(message) => check!(message, v2),
            }
            case.plan
                .control(fixture::peer::PeerControl::publication(peer.accept(&cleared)))
                .await;
            assert!(matches!(
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    case.protocol.explicit_use(captured),
                )
                .await
                .expect("focus clearing acknowledgement must release the connection owner")
                .unwrap(),
                PaneUseReply::ControlReady(_)
            ));
            case.runtime.shutdown().await.unwrap();
        }
    });
}

#[test]
fn desktop_native_context_invalidation_cancels_dispatch_stalled_behind_queue_traffic() {
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    executor.block_on(async {
        for major in [1, 2] {
            for reason in [
                ConnectionInvalidation::Publication,
                ConnectionInvalidation::Reconciliation,
            ] {
                let case = NativeContextFixture::new(major, held_context_publication).await;
                let mut peer = StrictContextPeer::default();
                case.retry_context(&mut peer, 0).await;
                let cancellation = case.subscription_cancellation(10_000);
                let count = case.context_requests().len();
                let sends = case.plan.sends_started.load(Ordering::SeqCst);
                case.pause_sends();
                let handle = case.protocol.handle(case.info.server).unwrap();
                let mut traffic = Box::pin(handle.dispatch(case.info, cancellation));
                tokio::select! {
                    biased;
                    result = &mut traffic => panic!("queue traffic bypassed its send gate: {result:?}"),
                    () = case.send_started_after(sends) => {},
                }
                case.pane
                    .update_context(spaceterm_client::pane_channel::PaneContextFacts {
                        focused_pane: true,
                        terminal_input_focus: true,
                        presentable: true,
                    })
                    .unwrap();
                let attempted = case.pane.state().borrow().context.get();
                let mut using = case.protocol.explicit_use(case.use_request());
                tokio::select! {
                    biased;
                    result = &mut using => panic!("context bypassed stalled queue traffic: {result:?}"),
                    () = std::future::ready(()) => {},
                }
                let binding = case.protocol.binding(case.info.server).unwrap();
                case.protocol.invalidate(&binding, reason);
                assert_eq!(
                    tokio::time::timeout(std::time::Duration::from_secs(1), using)
                        .await
                        .expect("invalidation must cancel dispatch without opening the send gate")
                        .unwrap_err()
                        .class(),
                    ClientFailureClass::StaleAuthority
                );
                assert_eq!(case.context_requests().len(), count);
                case.plan.client_send_gate.add_permits(4096);
                tokio::time::timeout(std::time::Duration::from_secs(5), traffic)
                    .await
                    .unwrap()
                    .unwrap();
                case.republish();
                let retry = case.retry_context(&mut peer, count).await;
                assert!(terminal_context_generations(&retry).1 > attempted);
                case.assert_unchanged_publication_does_not_replay().await;
                case.runtime.shutdown().await.unwrap();
            }
        }
    });
}

#[test]
fn desktop_native_context_timeout_bounds_dispatch_and_acknowledgement_together() {
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    executor.block_on(async {
        for major in [1, 2] {
            let case = NativeContextFixture::new(major, held_context_publication).await;
            let mut peer = StrictContextPeer::default();
            case.retry_context(&mut peer, 0).await;
            let first = case.subscription_cancellation(10_000);
            let second = case.subscription_cancellation(10_001);
            let count = case.context_requests().len();
            let sends = case.plan.sends_started.load(Ordering::SeqCst);
            case.pause_sends();
            let handle = case.protocol.handle(case.info.server).unwrap();
            let mut first = Box::pin(handle.dispatch(case.info, first));
            tokio::select! {
                biased;
                result = &mut first => panic!("queue traffic bypassed its send gate: {result:?}"),
                () = case.send_started_after(sends) => {},
            }
            let mut second = Box::pin(handle.dispatch(case.info, second));
            tokio::select! {
                biased;
                result = &mut second => panic!("second write bypassed the stalled writer: {result:?}"),
                () = std::future::ready(()) => {},
            }
            case.pane
                .update_context(spaceterm_client::pane_channel::PaneContextFacts {
                    focused_pane: true,
                    terminal_input_focus: true,
                    presentable: true,
                })
                .unwrap();
            let attempted = case.pane.state().borrow().context.get();
            let gate = case.plan.client_send_gate.clone();
            let release_first = tokio::spawn(async move {
                // Keep each write below the writer's 10-second transport deadline,
                // but keep the context behind queue traffic for its entire deadline.
                tokio::time::sleep(std::time::Duration::from_secs(8)).await;
                gate.add_permits(1);
            });
            assert_eq!(
                tokio::time::timeout(
                    std::time::Duration::from_secs(17),
                    case.protocol.explicit_use(case.use_request()),
                )
                .await
                .expect("context deadline must include time waiting for dispatch")
                .unwrap_err()
                .class(),
                ClientFailureClass::Unavailable
            );
            assert_eq!(case.context_requests().len(), count);
            assert!(matches!(
                *handle.state().borrow(),
                ConnectionState::Connected { info } if info.binding == case.info.binding
            ));
            case.plan.client_send_gate.add_permits(4096);
            release_first.await.unwrap();
            let (first, second) = tokio::join!(first, second);
            first.unwrap();
            second.unwrap();
            let retry = case.retry_context(&mut peer, count).await;
            assert!(terminal_context_generations(&retry).1 > attempted);
            case.assert_unchanged_publication_does_not_replay().await;
            case.runtime.shutdown().await.unwrap();
        }
    });
}

#[derive(Clone, Copy)]
enum InvalidContextEcho {
    Epoch,
    Binding,
    Generation,
    Focus,
    Input,
    Native,
}
fn change_context_echo(payload: &mut Payload, field: InvalidContextEcho) {
    macro_rules! profile {
        ($message:expr, $p:ident) => {{
            let Some($p::message::Kind::ClientContext(value)) = $message.kind.as_mut() else {
                panic!("context response")
            };
            match field {
                InvalidContextEcho::Epoch => value.epoch += 1,
                InvalidContextEcho::Binding => value.connection_generation += 1,
                InvalidContextEcho::Generation => value.context_generation += 1,
                InvalidContextEcho::Focus => value.focused_pane = None,
                InvalidContextEcho::Input => {
                    value.terminal_input_focus = !value.terminal_input_focus
                }
                InvalidContextEcho::Native => {
                    value.native_effects_eligible = !value.native_effects_eligible
                }
            }
        }};
    }
    match payload {
        Payload::Major1(message) => profile!(message, v1),
        Payload::Major2(message) => profile!(message, v2),
    }
}

#[test]
fn desktop_native_context_wire_handler_rejects_incoherent_outcomes_and_changed_echoes() {
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    executor.block_on(async {
        for major in [1, 2] {
            for (acknowledged, failure, field) in [
                (true, Some(2), None),
                (false, None, None),
                (false, Some(i32::MAX), None),
                (false, Some(1), None),
                (false, Some(2), Some(InvalidContextEcho::Epoch)),
                (false, Some(2), Some(InvalidContextEcho::Binding)),
                (false, Some(2), Some(InvalidContextEcho::Generation)),
                (false, Some(2), Some(InvalidContextEcho::Focus)),
                (false, Some(5), Some(InvalidContextEcho::Input)),
                (false, Some(5), Some(InvalidContextEcho::Native)),
            ] {
                let case = NativeContextFixture::new(major, held_context_publication).await;
                let request = case.pending_context().await;
                let mut response =
                    terminal_context_response(&request, acknowledged, failure).unwrap();
                if let Some(field) = field {
                    change_context_echo(&mut response, field);
                }
                let result = case
                    .protocol
                    .receive(case.info, response, &mut UnusedContextChannel)
                    .await;
                assert_eq!(result.unwrap_err().class(), ClientFailureClass::InvalidPeer);
                case.runtime.shutdown().await.unwrap();
            }
        }
    });
}
