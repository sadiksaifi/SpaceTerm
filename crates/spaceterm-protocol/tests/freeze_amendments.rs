#[expect(
    dead_code,
    reason = "shared authentication fixture also serves other suites"
)]
mod support;
use spaceterm_protocol::{
    ProtocolError,
    codec::Payload,
    wire::{v1, v2},
};
use support::{Fixture, entity, fence, operation_id};

fn capable_connections(
    major: u32,
) -> (
    spaceterm_protocol::connection::AuthenticatedConnection,
    spaceterm_protocol::connection::AuthenticatedConnection,
) {
    use spaceterm_protocol::authentication::{
        ClientHandshake, Established, HandshakeConfig, Purpose, ServerHandshake,
    };
    let fixture = Fixture::new();
    let config = HandshakeConfig {
        majors: vec![major],
        capabilities: vec![
            spaceterm_protocol::negotiation::CAPABILITY_SERVER_GIT,
            spaceterm_protocol::negotiation::CAPABILITY_CANONICAL_DOMAIN,
        ],
        ..Default::default()
    };
    let (client, hello) = ClientHandshake::begin(
        fixture.pin,
        fixture.principal,
        Purpose::Ordinary,
        config.clone(),
        fixture.device_key.clone(),
    )
    .unwrap();
    let (server, reply) = ServerHandshake::accept(
        fixture.principal.server,
        7,
        config,
        fixture.server_key.clone(),
        &fixture.pairings,
        &hello,
    )
    .unwrap();
    let (client, proof) = client.receive_server(&reply).unwrap();
    let (server, ready) = server.receive_device(&fixture.pairings, &proof).unwrap();
    let Established::Ordinary(client) = client.receive_ready(&ready).unwrap() else {
        unreachable!()
    };
    let Established::Ordinary(server) = server else {
        unreachable!()
    };
    (client, server)
}

#[test]
fn retained_control_loss_and_startup_results_round_trip_through_both_lookup_profiles() {
    macro_rules! check {
        ($p:ident, $major:ident) => {{
            for result in [
                $p::outcome::Result::ControlUnavailable($p::ControlUnavailableResult {
                    control: Some($p::control_unavailable_result::Control::Unclaimed(true)),
                }),
                $p::outcome::Result::ControlUnavailable($p::ControlUnavailableResult {
                    control: Some($p::control_unavailable_result::Control::Held(
                        $p::HeldControl {
                            client: Some(entity(4)),
                            generation: 7,
                        },
                    )),
                }),
                $p::outcome::Result::StartupAwaitingMeasurement(
                    $p::StartupAwaitingMeasurementResult {
                        intent: Some(operation_id()),
                    },
                ),
                $p::outcome::Result::SessionStarted(fence().session.unwrap()),
            ] {
                let outcome = $p::Outcome {
                    operation: Some(operation_id()),
                    publication_revision: 9,
                    result: Some(result),
                };
                // The lost response is recovered only through retained lookup, not
                // a new control/startup request with a replacement identity.
                let lookup = Payload::$major($p::Message {
                    kind: Some($p::message::Kind::OperationLookupResult(
                        $p::OperationLookupResult {
                            operation: Some(operation_id()),
                            request_digest: vec![8; 32],
                            disposition: Some($p::operation_lookup_result::Disposition::Completed(
                                Box::new(outcome.clone()),
                            )),
                        },
                    )),
                });
                let (mut client, mut server) = capable_connections(lookup.major());
                assert_eq!(client.decode(&server.encode(&lookup).unwrap()), Ok(lookup));
                let direct = Payload::$major($p::Message {
                    kind: Some($p::message::Kind::Outcome(outcome)),
                });
                let fixture = Fixture::new();
                let (_, mut server) = fixture.connect(&[direct.major()]);
                assert_eq!(server.encode(&direct), Err(ProtocolError::Negotiation));
            }
        }};
    }
    check!(v1, Major1);
    check!(v2, Major2);
}

#[test]
fn stamped_content_failure_does_not_terminate_another_multiplexed_read() {
    macro_rules! check {
        ($p:ident, $major:ident) => {{
            let stamp = |request| $p::QueryStamp {
                fence: Some(fence()),
                request,
                screen: 1,
                request_generation: 3,
                content_revision: 4,
                ..Default::default()
            };
            let failure = Payload::$major($p::Message {
                kind: Some($p::message::Kind::CopyEvent($p::CopyEvent {
                    item: Some($p::copy_event::Item::ContentFailure($p::ContentFailure {
                        stamp: Some(stamp(1)),
                        reason: $p::ContentReadFailure::Cancelled as i32,
                    })),
                })),
            });
            let chunk = Payload::$major($p::Message {
                kind: Some($p::message::Kind::CopyEvent($p::CopyEvent {
                    item: Some($p::copy_event::Item::Chunk($p::CopyChunk {
                        stamp: Some(stamp(2)),
                        transfer: 1,
                        part: 1,
                        offset: 0,
                        total: 1,
                        data: vec![b'x'],
                    })),
                })),
            });
            let complete = Payload::$major($p::Message {
                kind: Some($p::message::Kind::CopyEvent($p::CopyEvent {
                    item: Some($p::copy_event::Item::Complete($p::CopyCompletion {
                        stamp: Some(stamp(2)),
                        transfer: 1,
                        plain_bytes: 1,
                        plain_digest: vec![9; 32],
                        ..Default::default()
                    })),
                })),
            });
            let find_failure = Payload::$major($p::Message {
                kind: Some($p::message::Kind::FindEvent($p::FindEvent {
                    item: Some($p::find_event::Item::ContentFailure($p::ContentFailure {
                        stamp: Some(stamp(3)),
                        reason: $p::ContentReadFailure::StaleAuthority as i32,
                    })),
                })),
            });
            let (mut client, mut server) = capable_connections(failure.major());
            for payload in [failure, chunk, find_failure, complete] {
                assert_eq!(
                    client.decode(&server.encode(&payload).unwrap()),
                    Ok(payload)
                );
            }
        }};
    }
    check!(v1, Major1);
    check!(v2, Major2);
}

#[test]
fn canonical_graphics_manifest_preserves_cropped_offset_scaled_placement_and_first_request() {
    use spaceterm_core::{authority::*, content::*, content_queries::*, identity::*};
    macro_rules! check {
        ($p:ident, $major:ident) => {{
            let key = $p::ResourceId {
                session: fence().session,
                local_id: 6,
                generation: 7,
            };
            let stamp = $p::QueryStamp {
                fence: Some(fence()),
                request: 1,
                screen: 1,
                screen_generation: 5,
                content_revision: 7,
                ..Default::default()
            };
            let resource = $p::GraphicsResourceReference {
                resource: Some(key.clone()),
                width: 20,
                height: 10,
                rgba_bytes: 800,
                digest: vec![8; 32],
            };
            let placement = $p::GraphicPlacement {
                resource: Some(key),
                anchor: Some($p::PhysicalRowId {
                    screen: 1,
                    screen_generation: 5,
                    node_serial: 10,
                    page_row: 0,
                }),
                placement_id: 9,
                signed_column: -1,
                columns: 3,
                rows: 2,
                source_x: 2,
                source_y: 1,
                source_width: 10,
                source_height: 5,
                pixel_offset_x: 4,
                pixel_offset_y: 5,
                destination_width: 80,
                destination_height: 40,
                unicode_placeholder: true,
                z_index: -2,
                ..Default::default()
            };
            let payload = Payload::$major($p::Message {
                kind: Some($p::message::Kind::CanonicalSemanticPage(Box::new(
                    $p::CanonicalSemanticPage {
                        stamp: Some(stamp),
                        geometry: Some($p::Geometry {
                            rows: 24,
                            columns: 80,
                            cell_width_px: 8,
                            cell_height_px: 16,
                        }),
                        resources: vec![resource],
                        graphics: vec![placement],
                        complete: true,
                        ..Default::default()
                    },
                ))),
            });
            let (mut client, mut server) = capable_connections(payload.major());
            let decoded = client.decode(&server.encode(&payload).unwrap()).unwrap();
            assert_eq!(decoded, payload);
            let Payload::$major(message) = decoded else {
                unreachable!()
            };
            let Some($p::message::Kind::CanonicalSemanticPage(page)) = message.kind else {
                unreachable!()
            };
            let r = &page.resources[0];
            let g = &page.graphics[0];
            let s = r.resource.as_ref().unwrap().session.as_ref().unwrap();
            let pane = s.pane.as_ref().unwrap();
            let server = ServerId::new(
                pane.server
                    .as_ref()
                    .unwrap()
                    .value
                    .as_slice()
                    .try_into()
                    .unwrap(),
            );
            let session = SessionKey {
                pane: PaneId::new(server, pane.local_id).unwrap(),
                epoch: ServerEpoch::new(s.server_epoch),
                generation: SessionGeneration::new(s.generation),
            };
            let base = ContentBase {
                session,
                revision: ContentRevision::new(7),
                screen: Screen::Primary,
                screen_generation: 5,
                topology_revision: 0,
            };
            let reference = GraphicsResourceRef {
                key: GraphicsResourceKey {
                    session,
                    id: GraphicsResourceId::new(r.resource.as_ref().unwrap().local_id).unwrap(),
                    generation: r.resource.as_ref().unwrap().generation,
                },
                width: r.width,
                height: r.height,
                rgba_bytes: r.rgba_bytes,
                digest: r.digest.as_slice().try_into().unwrap(),
            };
            let anchor = g.anchor.as_ref().unwrap();
            let mut parts =
                ContentPageParts::new(base, page.geometry.as_ref().unwrap().columns as u16);
            parts.resources.push(reference);
            parts.placements.push(GraphicsPlacement {
                resource: reference.key,
                placement_id: g.placement_id,
                anchor: RowId {
                    screen: Screen::Primary,
                    screen_generation: anchor.screen_generation,
                    node_serial: anchor.node_serial,
                    page_row: anchor.page_row as u16,
                },
                column: g.signed_column,
                columns: g.columns as u16,
                rows: g.rows as u16,
                pixel_offset_x: g.pixel_offset_x,
                pixel_offset_y: g.pixel_offset_y,
                source: ImageRect {
                    x: g.source_x,
                    y: g.source_y,
                    width: g.source_width,
                    height: g.source_height,
                },
                destination_width: g.destination_width,
                destination_height: g.destination_height,
                unicode_placeholder: g.unicode_placeholder,
                z_index: g.z_index,
            });
            let page = ContentPage::new(parts).unwrap();
            assert_eq!(page.parts().placements[0].pixel_offset_x, 4);
            let client = ClientId::new(server, 1).unwrap();
            let grant = ControlGrant::new(session.pane, client, ControlGeneration::new(1)).unwrap();
            let domain_fence = EffectFence::new(
                session,
                grant,
                ConnectionBinding::new(client, ConnectionGeneration::new(1)),
                ContextGeneration::new(1),
                Some(base.revision),
            )
            .unwrap();
            let stamp = QueryStamp::new(
                domain_fence,
                base,
                ContentRequestId::new(1).unwrap(),
                RequestGeneration::new(0),
            )
            .unwrap();
            let first = GraphicsRequest::new(
                stamp,
                ResourceTransferId::new(1).unwrap(),
                page.parts().resources[0],
                0,
            )
            .unwrap();
            assert_eq!(first.resource(), reference);
        }};
    }
    check!(v1, Major1);
    check!(v2, Major2);
}

#[test]
fn repository_delta_publishes_upserts_and_deletions_without_active_workspace_state() {
    macro_rules! check {
        ($p:ident, $major:ident) => {{
            for delete in [false, true] {
                let payload = Payload::$major($p::Message {
                    kind: Some($p::message::Kind::OrganizationDelta(
                        $p::OrganizationDelta {
                            server: Some(support::id(1)),
                            epoch: 7,
                            base_revision: 4,
                            revision: 5,
                            repositories: if delete {
                                vec![]
                            } else {
                                vec![$p::RepositoryRecord {
                                    id: Some(entity(6)),
                                    directory: Some($p::ServerDirectoryValue {
                                        server: Some(support::id(1)),
                                        value: b"/repo".to_vec(),
                                    }),
                                    inventory_version: 1,
                                }]
                            },
                            deleted_repositories: if delete { vec![entity(6)] } else { vec![] },
                            complete: true,
                            ..Default::default()
                        },
                    )),
                });
                assert_eq!(
                    Payload::decode(payload.major(), &payload.encode().unwrap()),
                    Ok(payload)
                );
            }
        }};
    }
    check!(v1, Major1);
    check!(v2, Major2);
}

#[test]
fn measured_startup_and_resize_preserve_overflowing_cell_metrics_in_both_profiles() {
    macro_rules! check {
        ($p:ident, $major:ident) => {{
            let grid = $p::MeasuredGrid {
                rows: 24,
                columns: 1024,
                cell_width_px: 64,
                cell_height_px: 16,
            };
            let startup = Payload::$major($p::Message {
                kind: Some($p::message::Kind::StartupIntent($p::StartupIntent {
                    fence: Some(fence()),
                    original_intent: Some(operation_id()),
                    measured_grid: Some(grid.clone()),
                    ..Default::default()
                })),
            });
            let resize = Payload::$major($p::Message {
                kind: Some($p::message::Kind::RuntimeCommand($p::RuntimeCommand {
                    fence: Some(fence()),
                    request: 1,
                    action: Some($p::runtime_command::Action::MeasuredResize(grid)),
                })),
            });
            for payload in [startup, resize] {
                assert_eq!(
                    Payload::decode(payload.major(), &payload.encode().unwrap()),
                    Ok(payload.clone())
                );
                let (mut client, _) = Fixture::new().connect(&[payload.major()]);
                assert_eq!(client.encode(&payload), Err(ProtocolError::Negotiation));
            }
        }};
    }
    check!(v1, Major1);
    check!(v2, Major2);
}

#[test]
fn git_creation_profiles_preserve_new_existing_remote_and_explicit_or_template_choices() {
    macro_rules! check {
        ($p:ident, $dirs:ident, $major:ident) => {{
            use spaceterm_protocol::wire::directories::$dirs as d;
            use spaceterm_protocol::wire::git::$p as g;
            let scope = g::RepositoryScope {
                server: Some(support::id(1)),
                repository: Some(entity(6)),
            };
            let branch = |value: &str| g::BranchText {
                value: value.into(),
            };
            let directory = d::ServerDirectory {
                server: Some(support::id(1)),
                value: b"/work/topic".to_vec(),
            };
            for choice in [
                g::worktree_branch_choice::Choice::New(g::NewBranchChoice {
                    name: Some(branch("topic")),
                    base: Some(g::BranchReference {
                        reference: Some(g::branch_reference::Reference::Local(branch("main"))),
                    }),
                }),
                g::worktree_branch_choice::Choice::Existing(branch("topic")),
                g::worktree_branch_choice::Choice::Remote(g::RemoteBranchChoice {
                    remote: Some(branch("origin/topic")),
                    name: Some(branch("topic")),
                }),
            ] {
                for location in [
                    g::worktree_location_choice::Choice::Explicit(directory.clone()),
                    g::worktree_location_choice::Choice::ServerTemplate(g::ServerTemplateChoice {
                        expected_version: 7,
                    }),
                ] {
                    let draft = g::WorktreeCreateDraft {
                        repository: Some(scope.clone()),
                        branch: Some(g::WorktreeBranchChoice {
                            choice: Some(choice.clone()),
                        }),
                        location: Some(g::WorktreeLocationChoice {
                            choice: Some(location.clone()),
                        }),
                    };
                    let request = g::GitCreateAssessmentRequest {
                        draft: Some(draft.clone()),
                        ..Default::default()
                    };
                    let envelope = Payload::$major($p::Message {
                        kind: Some($p::message::Kind::GitQuery(g::GitQuery {
                            request: 1,
                            request_generation: 2,
                            scope: Some(scope.clone()),
                            action: Some(g::git_query::Action::AssessCreate(request)),
                            ..Default::default()
                        })),
                    });
                    let (mut client, mut server) = capable_connections(envelope.major());
                    assert_eq!(
                        server.decode(&client.encode(&envelope).unwrap()),
                        Ok(envelope)
                    );
                    let preview = g::WorktreeCreationPreview {
                        draft: Some(draft),
                        directory: Some(directory.clone()),
                        template_version: matches!(
                            location,
                            g::worktree_location_choice::Choice::ServerTemplate(_)
                        )
                        .then_some(7),
                    };
                    let envelope = Payload::$major($p::Message {
                        kind: Some($p::message::Kind::GitQueryResult(g::GitQueryResult {
                            request: 1,
                            request_generation: 2,
                            scope: Some(scope.clone()),
                            result: Some(g::git_query_result::Result::Assessment(
                                g::GitAssessmentPage {
                                    creation_preview: Some(Box::new(preview)),
                                    ..Default::default()
                                },
                            )),
                        })),
                    });
                    assert_eq!(
                        client.decode(&server.encode(&envelope).unwrap()),
                        Ok(envelope)
                    );
                }
            }
        }};
    }
    check!(v1, v1, Major1);
    check!(v2, v2, Major2);
}

#[test]
fn outcome_conversion_inventory_is_exhaustive_and_legacy_domain_variants_have_distinct_wire_arms() {
    use spaceterm_core::operations::Outcome;
    // Compiler-enforced inventory. This test does not install the track's codec.
    fn tag(value: &Outcome) -> u32 {
        match value {
            Outcome::Applied => 4,
            Outcome::CreatedWorkspace { .. } => 16,
            Outcome::CreatedTab { .. } => 17,
            Outcome::CreatedPane { .. } => 18,
            Outcome::CreatedWorktree { .. } => 19,
            Outcome::ControlAcquired(_) => 20,
            Outcome::ControlUnavailable(_) => 13,
            Outcome::StartupAwaitingMeasurement { .. } => 14,
            Outcome::SessionStarted(_) => 15,
            Outcome::Assessed { .. } => 21,
            Outcome::Conflict(_) => 5,
            Outcome::Failed(_) => 22,
            Outcome::CreatedWithControl(_) => 9,
            Outcome::ControlGranted(_) => 10,
            Outcome::Assessment(_) => 11,
            Outcome::Pairing(_) => 12,
            Outcome::Git(_) => 8,
        }
    }
    assert_eq!(tag(&Outcome::Applied), 4);
    macro_rules! check {
        ($p:ident, $major:ident) => {{
            let values = [
                $p::outcome::Result::LegacyCreatedWorkspace($p::LegacyCreatedWorkspaceResult {
                    workspace: Some(entity(1)),
                    tab: Some(entity(2)),
                    pane: Some(entity(3)),
                }),
                $p::outcome::Result::LegacyCreatedTab($p::LegacyCreatedTabResult {
                    tab: Some(entity(2)),
                    pane: Some(entity(3)),
                }),
                $p::outcome::Result::LegacyCreatedPane(entity(3)),
                $p::outcome::Result::LegacyCreatedWorktree(entity(4)),
                $p::outcome::Result::LegacyControlAcquired($p::LegacyControlAcquiredResult {
                    pane: Some(entity(3)),
                    client: Some(entity(4)),
                    generation: 7,
                }),
                $p::outcome::Result::LegacyAssessed($p::LegacyAssessedResult {
                    assessment: Some($p::AssessmentReference {
                        operation: Some(operation_id()),
                        digest: vec![8; 32],
                    }),
                    affected_panes: 4,
                    running_sessions: 2,
                }),
                $p::outcome::Result::OperationFailure($p::OperationFailureResult {
                    kind: $p::OperationFailureKind::OpRuntime as i32,
                    runtime_class: Some($p::RetainedRuntimeFailureClass::RuntimeFault as i32),
                }),
            ];
            for value in values {
                let payload = Payload::$major($p::Message {
                    kind: Some($p::message::Kind::Outcome($p::Outcome {
                        operation: Some(operation_id()),
                        publication_revision: 7,
                        result: Some(value),
                    })),
                });
                let (mut client, mut server) = capable_connections(payload.major());
                assert_eq!(
                    client.decode(&server.encode(&payload).unwrap()),
                    Ok(payload)
                );
            }
        }};
    }
    check!(v1, Major1);
    check!(v2, Major2);
}

#[test]
fn graphics_manifest_admission_bounds_resources_and_requires_complete_descriptors() {
    macro_rules! check {
        ($p:ident, $major:ident) => {{
            let resource = $p::GraphicsResourceReference {
                resource: Some($p::ResourceId {
                    session: fence().session,
                    local_id: 6,
                    generation: 1,
                }),
                width: 1,
                height: 1,
                rgba_bytes: 4,
                digest: vec![8; 32],
            };
            let page = |resources| {
                Payload::$major($p::Message {
                    kind: Some($p::message::Kind::CanonicalSemanticPage(Box::new(
                        $p::CanonicalSemanticPage {
                            stamp: Some($p::QueryStamp {
                                fence: Some(fence()),
                                request: 1,
                                screen: 1,
                                ..Default::default()
                            }),
                            geometry: Some($p::Geometry {
                                rows: 24,
                                columns: 80,
                                cell_width_px: 8,
                                cell_height_px: 16,
                            }),
                            resources,
                            ..Default::default()
                        },
                    ))),
                })
            };
            assert!(page(vec![resource.clone(); 128]).encode().is_ok());
            assert_eq!(
                page(vec![resource.clone(); 129]).encode(),
                Err(ProtocolError::LimitExceeded)
            );
            let mut missing_digest = resource.clone();
            missing_digest.digest.clear();
            assert_eq!(
                page(vec![missing_digest]).encode(),
                Err(ProtocolError::Malformed)
            );
            let mut missing_width = resource;
            missing_width.width = 0;
            assert_eq!(
                page(vec![missing_width]).encode(),
                Err(ProtocolError::Malformed)
            );
        }};
    }
    check!(v1, Major1);
    check!(v2, Major2);
}
#[test]
fn content_failure_requires_original_query_stamp_and_nonzero_classification() {
    macro_rules! check {
        ($p:ident, $major:ident) => {{
            for failure in [
                $p::ContentFailure {
                    stamp: None,
                    reason: $p::ContentReadFailure::Cancelled as i32,
                },
                $p::ContentFailure {
                    stamp: Some($p::QueryStamp {
                        fence: Some(fence()),
                        request: 1,
                        screen: 1,
                        ..Default::default()
                    }),
                    reason: 0,
                },
            ] {
                let payload = Payload::$major($p::Message {
                    kind: Some($p::message::Kind::FindEvent($p::FindEvent {
                        item: Some($p::find_event::Item::ContentFailure(failure)),
                    })),
                });
                assert_eq!(payload.encode(), Err(ProtocolError::Malformed));
            }
        }};
    }
    check!(v1, Major1);
    check!(v2, Major2);
}

#[test]
fn page_and_topology_results_preserve_multiplexed_rejection_and_resync() {
    macro_rules! check {
        ($p:ident, $major:ident) => {{
            let stamp = |request| $p::QueryStamp {
                fence: Some(fence()),
                request,
                request_generation: 3,
                content_revision: 4,
                topology_revision: 5,
                screen_generation: 6,
                screen: 1,
            };
            let (mut client, mut server) =
                capable_connections(if stringify!($p) == "v1" { 1 } else { 2 });
            for request in 1..=6 {
                let query = Payload::$major($p::Message {
                    kind: Some($p::message::Kind::ContentQuery($p::ContentQuery {
                        fence: Some(fence()),
                        kind: if matches!(request, 1 | 2 | 5) { 1 } else { 3 },
                        stamp: Some(Box::new(stamp(request))),
                        ..Default::default()
                    })),
                });
                assert_eq!(server.decode(&client.encode(&query).unwrap()), Ok(query));
            }
            let current = $p::ContentBase {
                session: fence().session,
                content_revision: 7,
                topology_revision: 8,
                screen_generation: 9,
                screen: 2,
            };
            let replies = [
                $p::content_read_response::Result::Rejected($p::ContentFailure {
                    stamp: Some(stamp(1)),
                    reason: $p::ContentReadFailure::InvalidRange as i32,
                }),
                $p::content_read_response::Result::Topology($p::TopologyPage {
                    stamp: Some(stamp(4)),
                    ..Default::default()
                }),
                $p::content_read_response::Result::ResyncRequired($p::ContentResyncRequired {
                    stamp: Some(stamp(3)),
                    current: Some(current.clone()),
                }),
                $p::content_read_response::Result::Page(Box::new($p::CanonicalSemanticPage {
                    stamp: Some(stamp(2)),
                    geometry: Some($p::Geometry {
                        rows: 24,
                        columns: 80,
                        cell_width_px: 8,
                        cell_height_px: 16,
                    }),
                    complete: true,
                    ..Default::default()
                })),
                $p::content_read_response::Result::ResyncRequired($p::ContentResyncRequired {
                    stamp: Some(stamp(5)),
                    current: Some(current),
                }),
                $p::content_read_response::Result::Rejected($p::ContentFailure {
                    stamp: Some(stamp(6)),
                    reason: $p::ContentReadFailure::StaleAuthority as i32,
                }),
            ];
            for result in replies {
                let payload = Payload::$major($p::Message {
                    kind: Some($p::message::Kind::ContentReadResponse(Box::new(
                        $p::ContentReadResponse {
                            result: Some(result),
                        },
                    ))),
                });
                // Each result retains its original request and current revision facts,
                // without closing the authenticated connection or relabeling another read.
                assert_eq!(
                    client.decode(&server.encode(&payload).unwrap()),
                    Ok(payload.clone())
                );
                assert_eq!(
                    client.encode(&payload),
                    Err(ProtocolError::UnexpectedMessage)
                );
                let fixture = Fixture::new();
                let (_, mut incapable) = fixture.connect(&[payload.major()]);
                assert_eq!(incapable.encode(&payload), Err(ProtocolError::Negotiation));
            }
        }};
    }
    check!(v1, Major1);
    check!(v2, Major2);
}

#[test]
fn page_result_admission_requires_correlation_and_current_resync_facts() {
    macro_rules! check {
        ($p:ident, $major:ident) => {{
            use prost::Message;
            use spaceterm_protocol::limits::NegotiatedLimits;
            let stamp = $p::QueryStamp {
                fence: Some(fence()),
                request: 1,
                screen: 1,
                ..Default::default()
            };
            for result in [
                $p::content_read_response::Result::Rejected($p::ContentFailure {
                    stamp: None,
                    reason: $p::ContentReadFailure::Cancelled as i32,
                }),
                $p::content_read_response::Result::ResyncRequired($p::ContentResyncRequired {
                    stamp: Some(stamp.clone()),
                    current: None,
                }),
            ] {
                let bytes = $p::Message {
                    kind: Some($p::message::Kind::ContentReadResponse(Box::new(
                        $p::ContentReadResponse {
                            result: Some(result),
                        },
                    ))),
                }
                .encode_to_vec();
                assert_eq!(
                    Payload::decode_with_limits(
                        if stringify!($p) == "v1" { 1 } else { 2 },
                        &bytes,
                        NegotiatedLimits::default()
                    ),
                    Err(ProtocolError::Malformed)
                );
            }
            let major = if stringify!($p) == "v1" { 1 } else { 2 };
            let empty = $p::Message {
                kind: Some($p::message::Kind::ContentReadResponse(Box::new(
                    $p::ContentReadResponse { result: None },
                ))),
            }
            .encode_to_vec();
            assert_eq!(
                Payload::decode(major, &empty),
                Err(ProtocolError::Malformed)
            );
            let rejected = $p::ContentReadResponse {
                result: Some($p::content_read_response::Result::Rejected(
                    $p::ContentFailure {
                        stamp: Some(stamp.clone()),
                        reason: $p::ContentReadFailure::Cancelled as i32,
                    },
                )),
            }
            .encode_to_vec();
            let resync = $p::ContentReadResponse {
                result: Some($p::content_read_response::Result::ResyncRequired(
                    $p::ContentResyncRequired {
                        stamp: Some(stamp),
                        current: Some($p::ContentBase {
                            session: fence().session,
                            screen: 1,
                            ..Default::default()
                        }),
                    },
                )),
            }
            .encode_to_vec();
            for second in [&rejected, &resync] {
                let mut competing = vec![0xfa, 0x02]; // Message tag 47, length-delimited.
                prost::encoding::encode_varint(
                    (rejected.len() + second.len()) as u64,
                    &mut competing,
                );
                competing.extend_from_slice(&rejected);
                competing.extend_from_slice(second);
                assert_eq!(
                    Payload::decode(major, &competing),
                    Err(ProtocolError::Malformed)
                );
            }
        }};
    }
    check!(v1, Major1);
    check!(v2, Major2);
}

#[test]
fn clear_and_cell_metrics_are_fenced_runtime_actions_in_both_majors() {
    macro_rules! check {
        ($p:ident, $major:ident) => {{
            let (mut client, mut server) =
                capable_connections(if stringify!($p) == "v1" { 1 } else { 2 });
            for action in [
                $p::runtime_command::Action::ClearScreenAndScrollback(
                    $p::ClearScreenAndScrollback {},
                ),
                $p::runtime_command::Action::CellPixelMetrics($p::CellPixelMetrics {
                    width: 65535,
                    height: 1,
                }),
            ] {
                let payload = Payload::$major($p::Message {
                    kind: Some($p::message::Kind::RuntimeCommand($p::RuntimeCommand {
                        fence: Some(fence()),
                        request: 31,
                        action: Some(action),
                    })),
                });
                assert_eq!(
                    server.decode(&client.encode(&payload).unwrap()),
                    Ok(payload.clone())
                );
                let fixture = Fixture::new();
                let (mut incapable, _) = fixture.connect(&[payload.major()]);
                assert_eq!(incapable.encode(&payload), Err(ProtocolError::Negotiation));
            }
        }};
    }
    check!(v1, Major1);
    check!(v2, Major2);
}

#[test]
fn git_queries_carry_unpinned_pane_workspace_and_distinct_worktree_targets() {
    use spaceterm_protocol::wire::git::{v1 as git_v1, v2 as git_v2};
    macro_rules! check {
        ($p:ident, $git:ident, $major:ident) => {{
            let targets = [
                $git::git_status_target::Target::Pane(entity(1)),
                $git::git_status_target::Target::Workspace(entity(2)),
                $git::git_status_target::Target::Worktree(entity(3)),
                $git::git_status_target::Target::Worktree(entity(4)),
            ];
            let (mut client, mut server) =
                capable_connections(if stringify!($p) == "v1" { 1 } else { 2 });
            for (index, target) in targets.into_iter().enumerate() {
                let payload = Payload::$major($p::Message {
                    kind: Some($p::message::Kind::GitQuery($git::GitQuery {
                        request: index as u64 + 1,
                        request_generation: 2,
                        // Discovery must work before a Pane knows its repository.
                        scope: (index >= 2).then(|| $git::RepositoryScope {
                            server: entity(9).server,
                            repository: Some(entity(9)),
                        }),
                        action: Some($git::git_query::Action::StatusTarget(
                            $git::GitStatusTarget {
                                target: Some(target),
                            },
                        )),
                        ..Default::default()
                    })),
                });
                assert_eq!(
                    server.decode(&client.encode(&payload).unwrap()),
                    Ok(payload)
                );
            }
            let payload = Payload::$major($p::Message {
                kind: Some($p::message::Kind::GitQuery($git::GitQuery {
                    request: 5,
                    request_generation: 2,
                    scope: None,
                    action: Some($git::git_query::Action::WorkspaceInventory(
                        $git::GitInventoryRequest {
                            workspace: Some(entity(2)),
                        },
                    )),
                    ..Default::default()
                })),
            });
            assert_eq!(
                server.decode(&client.encode(&payload).unwrap()),
                Ok(payload)
            );
        }};
    }
    check!(v1, git_v1, Major1);
    check!(v2, git_v2, Major2);
}

#[test]
fn every_typed_git_failure_survives_authenticated_query_responses() {
    use spaceterm_core::worktrees::GitFailure as DomainFailure;
    use spaceterm_protocol::wire::git::{v1 as git_v1, v2 as git_v2};
    macro_rules! check {
        ($p:ident, $git:ident, $major:ident) => {{
            // Exhaustive compiler-checked conversion inventory for the track owner.
            let wire_class = |class| match class {
                DomainFailure::Unauthorized => $git::GitFailure::GitUnauthorized,
                DomainFailure::WrongServer => $git::GitFailure::GitWrongServer,
                DomainFailure::TargetGone => $git::GitFailure::GitTargetGone,
                DomainFailure::Offline => $git::GitFailure::GitOffline,
                DomainFailure::Busy => $git::GitFailure::GitBusy,
                DomainFailure::Capacity => $git::GitFailure::GitCapacity,
                DomainFailure::ExpiredAssessment => $git::GitFailure::GitExpiredAssessment,
                DomainFailure::StaleAssessment => $git::GitFailure::GitStaleAssessment,
                DomainFailure::MainWorktree => $git::GitFailure::GitMainWorktree,
                DomainFailure::LockedWorktree => $git::GitFailure::GitLockedWorktree,
                DomainFailure::UnlistedWorktree => $git::GitFailure::GitUnlistedWorktree,
                DomainFailure::HoldsPinnedDirectory => $git::GitFailure::GitHoldsPinnedDirectory,
                DomainFailure::NoFallback => $git::GitFailure::GitNoFallback,
                DomainFailure::InvalidBranch => $git::GitFailure::GitInvalidBranch,
                DomainFailure::InvalidTemplate => $git::GitFailure::GitInvalidTemplate,
                DomainFailure::InvalidDirectory => $git::GitFailure::GitInvalidDirectory,
                DomainFailure::OccupiedTarget => $git::GitFailure::GitOccupiedTarget,
                DomainFailure::BranchExists => $git::GitFailure::GitBranchExists,
                DomainFailure::BranchMissing => $git::GitFailure::GitBranchMissing,
                DomainFailure::BranchCheckedOut => $git::GitFailure::GitBranchCheckedOut,
                DomainFailure::IdentityReplaced => $git::GitFailure::GitIdentityReplaced,
                DomainFailure::GitStateChanged => $git::GitFailure::GitGitStateChanged,
                DomainFailure::Unchecked => $git::GitFailure::GitUnchecked,
                DomainFailure::ToolMissing => $git::GitFailure::GitToolMissing,
                DomainFailure::ToolTooOld => $git::GitFailure::GitToolTooOld,
                DomainFailure::CancelledRead => $git::GitFailure::GitCancelledRead,
                DomainFailure::TimedOutRead => $git::GitFailure::GitTimedOutRead,
                DomainFailure::InvalidResponse => $git::GitFailure::GitInvalidResponse,
                DomainFailure::OutputTooLarge => $git::GitFailure::GitOutputTooLarge,
                DomainFailure::WriteFailed => $git::GitFailure::GitWriteFailed,
                DomainFailure::ExternalStateUncertain => {
                    $git::GitFailure::GitExternalStateUncertain
                }
                DomainFailure::RecoveryNeedsReview => $git::GitFailure::GitRecoveryNeedsReview,
                DomainFailure::StorageLocked => $git::GitFailure::GitStorageLocked,
                DomainFailure::StorageUnavailable => $git::GitFailure::GitStorageUnavailable,
                DomainFailure::UnsafeStorage => $git::GitFailure::GitUnsafeStorage,
                DomainFailure::StorageFailure => $git::GitFailure::GitStorageFailure,
                DomainFailure::IndeterminateDurability => {
                    $git::GitFailure::GitIndeterminateDurability
                }
            };
            let classes = [
                DomainFailure::Unauthorized,
                DomainFailure::WrongServer,
                DomainFailure::TargetGone,
                DomainFailure::Offline,
                DomainFailure::Busy,
                DomainFailure::Capacity,
                DomainFailure::ExpiredAssessment,
                DomainFailure::StaleAssessment,
                DomainFailure::MainWorktree,
                DomainFailure::LockedWorktree,
                DomainFailure::UnlistedWorktree,
                DomainFailure::HoldsPinnedDirectory,
                DomainFailure::NoFallback,
                DomainFailure::InvalidBranch,
                DomainFailure::InvalidTemplate,
                DomainFailure::InvalidDirectory,
                DomainFailure::OccupiedTarget,
                DomainFailure::BranchExists,
                DomainFailure::BranchMissing,
                DomainFailure::BranchCheckedOut,
                DomainFailure::IdentityReplaced,
                DomainFailure::GitStateChanged,
                DomainFailure::Unchecked,
                DomainFailure::ToolMissing,
                DomainFailure::ToolTooOld,
                DomainFailure::CancelledRead,
                DomainFailure::TimedOutRead,
                DomainFailure::InvalidResponse,
                DomainFailure::OutputTooLarge,
                DomainFailure::WriteFailed,
                DomainFailure::ExternalStateUncertain,
                DomainFailure::RecoveryNeedsReview,
                DomainFailure::StorageLocked,
                DomainFailure::StorageUnavailable,
                DomainFailure::UnsafeStorage,
                DomainFailure::StorageFailure,
                DomainFailure::IndeterminateDurability,
            ];
            let (mut client, mut server) =
                capable_connections(if stringify!($p) == "v1" { 1 } else { 2 });
            for (index, class) in classes.into_iter().enumerate() {
                assert_eq!(wire_class(class) as usize, index + 1);
                let payload = Payload::$major($p::Message {
                    kind: Some($p::message::Kind::GitQueryResult($git::GitQueryResult {
                        request: 7,
                        request_generation: 8,
                        scope: None,
                        result: Some($git::git_query_result::Result::TypedFailure(
                            wire_class(class) as i32,
                        )),
                    })),
                });
                assert_eq!(
                    client.decode(&server.encode(&payload).unwrap()),
                    Ok(payload)
                );
            }
        }};
    }
    check!(v1, git_v1, Major1);
    check!(v2, git_v2, Major2);
}

#[test]
fn runtime_command_conversion_inventory_has_a_declared_route_for_every_variant() {
    use spaceterm_core::geometry::CellPixels;
    use spaceterm_terminal::runtime_contracts::RuntimeCommand;
    // This inventory is a declaration fixture, not a production converter.
    // ResolvePaste and CompleteNativeRequest use the existing NativeCompletion envelope.
    let route = |command: &RuntimeCommand| match command {
        RuntimeCommand::Key(_) => (11, 3),
        RuntimeCommand::InputMethodCommit(_) => (11, 4),
        RuntimeCommand::Pointer(_) | RuntimeCommand::Wheel(_) => (11, 6),
        RuntimeCommand::Resize(_) => (11, 10),
        RuntimeCommand::Appearance(_) => (11, 8),
        RuntimeCommand::FocusReport(_) => (11, 9),
        RuntimeCommand::CellPixelMetrics(_) => (11, 12),
        RuntimeCommand::ClearScreenAndScrollback => (11, 11),
        RuntimeCommand::RequestPaste(_) => (11, 5),
        RuntimeCommand::ResolvePaste(_, _) => (19, 7),
        RuntimeCommand::CompleteNativeRequest(_) => (19, 6),
    };
    assert_eq!(route(&RuntimeCommand::ClearScreenAndScrollback), (11, 11));
    assert_eq!(
        route(&RuntimeCommand::CellPixelMetrics(
            CellPixels::new(65535, 1).unwrap()
        )),
        (11, 12)
    );
}

#[test]
fn new_runtime_metrics_and_git_selectors_reject_incomplete_or_out_of_bound_fields() {
    use spaceterm_protocol::wire::git::{v1 as git_v1, v2 as git_v2};
    macro_rules! check {
        ($p:ident, $git:ident, $major:ident) => {{
            for (width, expected) in [
                (0, ProtocolError::Malformed),
                (65536, ProtocolError::Malformed),
            ] {
                let payload = Payload::$major($p::Message {
                    kind: Some($p::message::Kind::RuntimeCommand($p::RuntimeCommand {
                        fence: Some(fence()),
                        request: 1,
                        action: Some($p::runtime_command::Action::CellPixelMetrics(
                            $p::CellPixelMetrics { width, height: 16 },
                        )),
                    })),
                });
                assert_eq!(payload.encode(), Err(expected));
            }
            for action in [
                $git::git_query::Action::StatusTarget($git::GitStatusTarget { target: None }),
                $git::git_query::Action::WorkspaceInventory($git::GitInventoryRequest {
                    workspace: None,
                }),
            ] {
                let payload = Payload::$major($p::Message {
                    kind: Some($p::message::Kind::GitQuery($git::GitQuery {
                        request: 1,
                        action: Some(action),
                        ..Default::default()
                    })),
                });
                assert_eq!(payload.encode(), Err(ProtocolError::Malformed));
            }
        }};
    }
    check!(v1, git_v1, Major1);
    check!(v2, git_v2, Major2);
}
