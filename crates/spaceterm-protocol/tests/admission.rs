#[expect(
    dead_code,
    reason = "the shared fixture also supports handshake conformance tests"
)]
mod support;

use prost::Message;
use spaceterm_protocol::{
    ProtocolError,
    codec::Payload,
    limits::{
        MAX_CLIPBOARD_BYTES, MAX_FRAME_BYTES, MAX_PASTE_BYTES, MAX_RESOURCE_CHUNK_BYTES,
        NegotiatedLimits,
    },
    wire::{common, v1, v2},
};
use support::{Fixture, entity, fence, operation_id};

fn style1() -> v1::Style {
    v1::Style {
        foreground: Some(v1::Color {
            value: Some(v1::color::Value::DefaultColor(true)),
        }),
        background: Some(v1::Color {
            value: Some(v1::color::Value::DefaultColor(true)),
        }),
        ..Default::default()
    }
}
fn style2() -> v2::Style {
    v2::Style {
        foreground: Some(v2::Color {
            value: Some(v2::color::Value::DefaultColor(true)),
        }),
        background: Some(v2::Color {
            value: Some(v2::color::Value::DefaultColor(true)),
        }),
        ..Default::default()
    }
}
fn page1(rows: usize, cells: usize) -> v1::Message {
    v1::Message {
        kind: Some(v1::message::Kind::SemanticPage(v1::SemanticPage {
            fence: Some(fence()),
            request_generation: 1,
            content_revision: 1,
            screen_generation: 1,
            topology_revision: 1,
            rows: (0..rows)
                .map(|index| v1::SemanticRow {
                    id: index as u64 + 1,
                    revision: 1,
                    soft_wrap: false,
                    cells: vec![
                        v1::StyledCell {
                            grapheme: "a".into(),
                            width: 1,
                            style: Some(style1()),
                            ..Default::default()
                        };
                        cells
                    ],
                    physical_row: None,
                    start_column: 0,
                    end_column: 0,
                    columns: 0,
                })
                .collect(),
            complete: true,
            ..Default::default()
        })),
    }
}
fn page2(rows: usize, cells: usize) -> v2::Message {
    v2::Message {
        kind: Some(v2::message::Kind::SemanticPage(v2::SemanticPage {
            fence: Some(fence()),
            request_generation: 1,
            content_revision: 1,
            screen_generation: 1,
            topology_revision: 1,
            rows: (0..rows)
                .map(|index| v2::SemanticRow {
                    id: index as u64 + 1,
                    revision: 1,
                    soft_wrap: false,
                    runs: vec![v2::StyledRun {
                        style: Some(style2()),
                        hyperlink_id: 0,
                        graphemes: vec![
                            v2::Grapheme {
                                text: "a".into(),
                                width: 1,
                                spacer: false,
                                wrap_head: false,
                            };
                            cells
                        ],
                    }],
                    physical_row: None,
                    start_column: 0,
                    end_column: 0,
                    columns: 0,
                })
                .collect(),
            complete: true,
            ..Default::default()
        })),
    }
}

fn alternating_style_page2(rows: usize, cells: usize) -> v2::Message {
    let mut message = page2(rows, 0);
    let Some(v2::message::Kind::SemanticPage(page)) = &mut message.kind else {
        panic!("semantic page")
    };
    for (index, row) in page.rows.iter_mut().enumerate() {
        row.runs = (0..cells)
            .map(|column| {
                let mut style = style2();
                style.foreground = Some(v2::Color {
                    value: Some(v2::color::Value::Rgb(if (index + column) % 2 == 0 {
                        0xff_6600
                    } else {
                        0x00_66ff
                    })),
                });
                v2::StyledRun {
                    style: Some(style),
                    hyperlink_id: (index * cells + column) as u64 % 1024 + 1,
                    graphemes: vec![v2::Grapheme {
                        text: "a".into(),
                        width: 1,
                        ..Default::default()
                    }],
                }
            })
            .collect();
    }
    // Exercise every repeated metadata collection at its own limit as well.
    page.color_overrides = (0..256)
        .map(|slot| v2::ColorOverride { slot, rgb: slot })
        .collect();
    page.color_state = Some(v2::ColorState {
        default_foreground: Some(0xff_ffff),
        default_background: Some(0),
        cursor_color: Some(0x01_0203),
        reverse_colors: true,
    });
    page.hyperlinks = (1..=1024)
        .map(|id| v2::Hyperlink {
            id,
            uri: "https://example.org".into(),
        })
        .collect();
    page.graphics = (1..=1024)
        .map(|local_id| v2::GraphicPlacement {
            resource: Some(v2::ResourceId {
                session: fence().session,
                local_id,
                generation: 0,
            }),
            row_id: 1,
            columns: 1,
            rows: 1,
            ..Default::default()
        })
        .collect();
    let cell_ref = v2::CellRef {
        session: fence().session,
        screen_generation: 1,
        row_id: 1,
        row_revision: 1,
        column: 0,
        physical_row: None,
    };
    page.matches = vec![
        v2::FindMatch {
            start: Some(cell_ref.clone()),
            end: Some(cell_ref)
        };
        1024
    ];
    message
}

#[test]
fn maximum_alternating_style_pages_authenticate_with_all_bounded_metadata() {
    let fixture = Fixture::new();
    let (mut client, mut server) = fixture.connect(&[2, 1]);
    for (rows, cells) in [(128, 128), (256, 64)] {
        let message = alternating_style_page2(rows, cells);
        assert!(message.encoded_len() < MAX_FRAME_BYTES);
        let payload = Payload::Major2(message);
        let envelope = server
            .encode(&payload)
            .expect("a page at the cell limit must be admitted");
        assert_eq!(client.decode(&envelope), Ok(payload));
    }
}

#[test]
fn alternating_style_pages_above_the_cell_limit_are_rejected_before_generated_decode() {
    let mut message = alternating_style_page2(128, 128);
    let Some(v2::message::Kind::SemanticPage(page)) = &mut message.kind else {
        panic!("semantic page")
    };
    let extra = page.rows[0].runs[0].clone();
    page.rows[0].runs.push(extra);
    assert_eq!(
        Payload::decode(2, &message.encode_to_vec()),
        Err(ProtocolError::LimitExceeded)
    );
    let fixture = Fixture::new();
    let (_, mut server) = fixture.connect(&[2, 1]);
    assert_eq!(
        server.encode(&Payload::Major2(message)),
        Err(ProtocolError::LimitExceeded)
    );
}

#[test]
fn page_metadata_lists_keep_their_own_limits_with_the_larger_structural_budget() {
    let message = alternating_style_page2(1, 1);
    for field in ["color_overrides", "hyperlinks", "graphics", "matches"] {
        let mut message = message.clone();
        let Some(v2::message::Kind::SemanticPage(page)) = &mut message.kind else {
            panic!("semantic page")
        };
        match field {
            "color_overrides" => page.color_overrides.push(page.color_overrides[0]),
            "hyperlinks" => page.hyperlinks.push(page.hyperlinks[0].clone()),
            "graphics" => page.graphics.push(page.graphics[0].clone()),
            "matches" => page.matches.push(page.matches[0].clone()),
            _ => unreachable!(),
        }
        assert_eq!(
            Payload::decode(2, &message.encode_to_vec()),
            Err(ProtocolError::LimitExceeded)
        );
    }
}

#[test]
fn repeated_rows_and_aggregate_cells_are_rejected_before_generated_decode() {
    for payload in [
        Payload::Major1(page1(257, 1)),
        Payload::Major2(page2(257, 1)),
        Payload::Major1(page1(2, 8193)),
        Payload::Major2(page2(2, 8193)),
    ] {
        let bytes = match &payload {
            Payload::Major1(value) => value.encode_to_vec(),
            Payload::Major2(value) => value.encode_to_vec(),
        };
        assert_eq!(
            Payload::decode(payload.major(), &bytes),
            Err(ProtocolError::LimitExceeded)
        );
    }
    for payload in [
        Payload::Major1(page1(256, 1)),
        Payload::Major2(page2(256, 1)),
        Payload::Major1(page1(1, 16_384)),
        Payload::Major2(page2(1, 16_384)),
    ] {
        assert_eq!(
            Payload::decode(payload.major(), &payload.encode().unwrap()),
            Ok(payload)
        );
    }
}

#[test]
fn semantic_pages_preserve_program_default_and_cursor_color_state_in_both_majors() {
    // An independent wire fixture pins the color-state field numbers in both profiles.
    #[derive(Clone, PartialEq, Message)]
    struct ColorState {
        #[prost(uint32, optional, tag = "1")]
        default_foreground: Option<u32>,
        #[prost(uint32, optional, tag = "2")]
        default_background: Option<u32>,
        #[prost(uint32, optional, tag = "3")]
        cursor_color: Option<u32>,
        #[prost(bool, tag = "4")]
        reverse_colors: bool,
    }
    #[derive(Clone, PartialEq, Message)]
    struct PageColorState {
        #[prost(message, optional, tag = "18")]
        color_state: Option<ColorState>,
    }
    #[derive(Clone, PartialEq, Message)]
    struct SemanticPageMessage {
        #[prost(bytes, tag = "14")]
        page: Vec<u8>,
    }

    let fixture = Fixture::new();
    for major in [1, 2] {
        let (mut client, mut server) = fixture.connect(if major == 1 { &[1] } else { &[2, 1] });
        for state in [
            ColorState {
                default_foreground: Some(0x01_0203),
                default_background: Some(0),
                cursor_color: Some(0x04_0506),
                reverse_colors: true,
            },
            ColorState {
                default_foreground: None,
                default_background: Some(0x07_0809),
                cursor_color: None,
                reverse_colors: true,
            },
            ColorState {
                default_foreground: None,
                default_background: None,
                cursor_color: None,
                reverse_colors: false,
            },
        ] {
            let mut page = if major == 1 {
                let Some(v1::message::Kind::SemanticPage(page)) = page1(1, 1).kind else {
                    panic!("semantic page")
                };
                page.encode_to_vec()
            } else {
                let Some(v2::message::Kind::SemanticPage(page)) = page2(1, 1).kind else {
                    panic!("semantic page")
                };
                page.encode_to_vec()
            };
            page.extend(
                PageColorState {
                    color_state: Some(state),
                }
                .encode_to_vec(),
            );
            let bytes = SemanticPageMessage { page }.encode_to_vec();
            let payload =
                Payload::decode(major, &bytes).expect("program color state must be admitted");
            assert_eq!(payload.encode().unwrap(), bytes);
            assert_eq!(
                client.decode(&server.encode(&payload).unwrap()),
                Ok(payload)
            );
        }
    }
}

#[test]
fn semantic_color_channels_enforce_24_bit_rgb_in_both_majors() {
    macro_rules! profile {
        ($profile:ident, $variant:ident, $make_page:ident) => {{
            let mut message = $make_page(1, 1);
            let Some($profile::message::Kind::SemanticPage(page)) = &mut message.kind else {
                panic!("semantic page")
            };
            page.color_state = Some($profile::ColorState {
                default_foreground: Some(0xff_ffff),
                default_background: Some(0),
                cursor_color: Some(0xff_ffff),
                reverse_colors: false,
            });
            let payload = Payload::$variant(message.clone());
            assert_eq!(
                Payload::decode(payload.major(), &payload.encode().unwrap()),
                Ok(payload)
            );
            for state in [
                $profile::ColorState {
                    default_foreground: Some(0x0100_0000),
                    ..Default::default()
                },
                $profile::ColorState {
                    default_background: Some(0x0100_0000),
                    ..Default::default()
                },
                $profile::ColorState {
                    cursor_color: Some(0x0100_0000),
                    ..Default::default()
                },
            ] {
                let Some($profile::message::Kind::SemanticPage(page)) = &mut message.kind else {
                    panic!("semantic page")
                };
                page.color_state = Some(state);
                let payload = Payload::$variant(message.clone());
                assert_eq!(
                    Payload::decode(payload.major(), &message.encode_to_vec()),
                    Err(ProtocolError::Malformed)
                );
                assert_eq!(payload.encode(), Err(ProtocolError::Malformed));
            }
        }};
    }
    profile!(v1, Major1, page1);
    profile!(v2, Major2, page2);
}

#[test]
fn duplicate_oneof_tags_and_singular_fields_do_not_merge_or_replace_payloads() {
    let message = v1::Message {
        kind: Some(v1::message::Kind::ManagementRequest(
            v1::ManagementRequest {
                request: 1,
                action: 1,
                ..Default::default()
            },
        )),
    }
    .encode_to_vec();
    let duplicated = [message.as_slice(), message.as_slice()].concat();
    assert_eq!(
        Payload::decode(1, &duplicated),
        Err(ProtocolError::Malformed)
    );
    // RuntimeCommand.fence contains duplicate session fields; prost normally merges them.
    let field = common::EffectFence {
        session: Some(common::Session {
            pane: Some(entity(1)),
            server_epoch: 7,
            generation: 1,
        }),
        ..Default::default()
    }
    .encode_to_vec();
    let malformed_fence = [field.as_slice(), field.as_slice()].concat();
    let mut command = vec![0x0a, malformed_fence.len() as u8];
    command.extend(malformed_fence);
    let mut message = vec![0x5a, command.len() as u8];
    message.extend(command);
    assert_eq!(Payload::decode(1, &message), Err(ProtocolError::Malformed));
}

#[test]
fn malformed_wire_fields_unknown_enums_and_missing_identity_are_rejected() {
    for bytes in [
        vec![],
        vec![0],
        vec![0x0a, 3, 0],
        vec![0x80; 10],
        vec![0xfa, 0x7f, 0],
        vec![0x08, 1],
    ] {
        assert_eq!(Payload::decode(1, &bytes), Err(ProtocolError::Malformed));
    }
    let request = v1::Message {
        kind: Some(v1::message::Kind::Operation(v1::Operation {
            id: Some(operation_id()),
            action: Some(v1::operation::Action::CreatePane(v1::CreatePane {
                tab: Some(entity(1)),
                source: Some(entity(2)),
                edge: 99,
            })),
        })),
    };
    assert_eq!(
        Payload::decode(1, &request.encode_to_vec()),
        Err(ProtocolError::Malformed)
    );
    let mut request = request;
    let Some(v1::message::Kind::Operation(operation)) = &mut request.kind else {
        panic!()
    };
    operation.id = None;
    assert_eq!(
        Payload::decode(1, &request.encode_to_vec()),
        Err(ProtocolError::Malformed)
    );
}

#[test]
fn malformed_semantic_width_and_spacer_topology_are_rejected() {
    let mut page = page1(1, 1);
    let Some(v1::message::Kind::SemanticPage(content)) = &mut page.kind else {
        panic!()
    };
    content.rows[0].cells[0].width = 3;
    assert_eq!(
        Payload::decode(1, &page.encode_to_vec()),
        Err(ProtocolError::Malformed)
    );
    let mut page = page2(1, 1);
    let Some(v2::message::Kind::SemanticPage(content)) = &mut page.kind else {
        panic!()
    };
    content.rows[0].runs[0].graphemes[0].spacer = true;
    assert_eq!(
        Payload::decode(2, &page.encode_to_vec()),
        Err(ProtocolError::Malformed)
    );
}

#[test]
fn paste_clipboard_and_resource_chunks_have_data_boundaries_in_both_majors() {
    let fixture = Fixture::new();
    for major in [1, 2] {
        let (mut client, mut server) = fixture.connect(if major == 1 { &[1] } else { &[2, 1] });
        macro_rules! payloads {
            ($profile:ident, $variant:ident) => {{
                let paste = |length| {
                    Payload::$variant($profile::Message {
                        kind: Some($profile::message::Kind::RuntimeCommand(
                            $profile::RuntimeCommand {
                                fence: Some(fence()),
                                request: 1,
                                action: Some($profile::runtime_command::Action::Paste(
                                    $profile::Paste {
                                        text: vec![b'x'; length],
                                        approval: Vec::new(),
                                    },
                                )),
                            },
                        )),
                    })
                };
                let clipboard = |length| {
                    Payload::$variant($profile::Message {
                        kind: Some($profile::message::Kind::NativeCompletion(
                            $profile::NativeCompletion {
                                fence: Some(fence()),
                                request: 1,
                                allowed: true,
                                clipboard_text: vec![b'x'; length],
                                failure: None,
                                ..Default::default()
                            },
                        )),
                    })
                };
                let resource = |length| {
                    Payload::$variant($profile::Message {
                        kind: Some($profile::message::Kind::ResourceChunk(
                            $profile::ResourceChunk {
                                fence: Some(fence()),
                                resource: Some($profile::ResourceId {
                                    session: fence().session,
                                    local_id: 1,
                                    generation: 0,
                                }),
                                transfer: 1,
                                total_bytes: length as u64,
                                data: vec![0; length],
                                digest: vec![1; 32],
                                complete: true,
                                format: $profile::ResourceFormat::Png as i32,
                                ..Default::default()
                            },
                        )),
                    })
                };
                for (make, maximum) in [
                    (&paste as &dyn Fn(usize) -> Payload, MAX_PASTE_BYTES),
                    (&clipboard, MAX_CLIPBOARD_BYTES),
                ] {
                    let value = make(maximum);
                    assert_eq!(server.decode(&client.encode(&value).unwrap()), Ok(value));
                    assert_eq!(
                        client.encode(&make(maximum + 1)),
                        Err(ProtocolError::LimitExceeded)
                    );
                }
                let value = resource(MAX_RESOURCE_CHUNK_BYTES);
                assert_eq!(client.decode(&server.encode(&value).unwrap()), Ok(value));
                assert_eq!(
                    server.encode(&resource(MAX_RESOURCE_CHUNK_BYTES + 1)),
                    Err(ProtocolError::LimitExceeded)
                );
                let value = paste(17);
                assert_eq!(
                    value.encode_with_limits(NegotiatedLimits {
                        paste_bytes: 16,
                        ..Default::default()
                    }),
                    Err(ProtocolError::LimitExceeded)
                );
            }};
        }
        if major == 1 {
            payloads!(v1, Major1);
        } else {
            payloads!(v2, Major2);
        }
    }
}

#[test]
fn management_limit_comes_from_message_kind_and_includes_serialized_payload_overhead() {
    let mut message = v1::Message {
        kind: Some(v1::message::Kind::DirectoryPage(v1::DirectoryPage {
            request: 1,
            entries: vec![v1::DirectoryEntry {
                name: "abcd".into(),
                directory: true,
                selectable: true,
            }],
            ..Default::default()
        })),
    };
    let limit = message.encoded_len();
    let limits = NegotiatedLimits {
        management_bytes: limit,
        ..Default::default()
    };
    assert!(
        Payload::Major1(message.clone())
            .encode_with_limits(limits)
            .is_ok()
    );
    let Some(v1::message::Kind::DirectoryPage(page)) = &mut message.kind else {
        panic!()
    };
    page.entries[0].name.push('e');
    assert_eq!(message.encoded_len(), limit + 1);
    assert_eq!(
        Payload::Major1(message).encode_with_limits(limits),
        Err(ProtocolError::LimitExceeded)
    );
}

#[test]
fn frame_limit_includes_authentication_overhead_not_only_the_semantic_payload() {
    let fixture = Fixture::new();
    let (_, mut server) = fixture.connect(&[2, 1]);
    let mut page = page2(1, 16_384);
    let Some(v2::message::Kind::SemanticPage(content)) = &mut page.kind else {
        panic!()
    };
    for cell in &mut content.rows[0].runs[0].graphemes {
        cell.text = format!("a{}", "\u{301}".repeat(59));
    }
    // Fill the valid semantic payload to just below the frame ceiling. Each
    // grapheme remains below its own byte bound and counts remain unchanged.
    let deficit = MAX_FRAME_BYTES - page.encoded_len() - 1;
    let Some(v2::message::Kind::SemanticPage(content)) = &mut page.kind else {
        panic!()
    };
    let mut remaining = deficit;
    for cell in &mut content.rows[0].runs[0].graphemes {
        let added = if remaining == 4 { 2 } else { remaining.min(3) };
        if added == 2 {
            cell.text.push('\u{301}');
        } else if added == 3 {
            cell.text.push('\u{20dd}');
        } else {
            panic!("grapheme byte budget");
        }
        remaining -= added;
        if remaining == 0 {
            break;
        }
    }
    assert_eq!(page.encoded_len(), MAX_FRAME_BYTES - 1);
    let payload = Payload::Major2(page);
    assert!(payload.encode().is_ok());
    assert_eq!(server.encode(&payload), Err(ProtocolError::LimitExceeded));
}

#[test]
fn a_styled_cell_cannot_pack_several_graphemes_into_one_column() {
    let mut page = page2(1, 1);
    let Some(v2::message::Kind::SemanticPage(content)) = &mut page.kind else {
        panic!()
    };
    content.rows[0].runs[0].graphemes[0].text = "ab".into();
    assert_eq!(
        Payload::decode(2, &page.encode_to_vec()),
        Err(ProtocolError::Malformed)
    );
}

#[test]
fn portable_key_events_preserve_repeat_consumed_modifiers_and_absent_text() {
    macro_rules! profile {
        ($profile:ident, $variant:ident) => {{
            let key = $profile::Key {
                code: common::PhysicalKey::KeyArrowUp as i32,
                modifiers: 5,
                text: None,
                action: common::KeyAction::KeyRepeat as i32,
                logical_key: "ArrowUp".into(),
                consumed_modifiers: 1,
                unshifted_codepoint: 0,
                option_as_alt: true,
            };
            let value = Payload::$variant($profile::Message {
                kind: Some($profile::message::Kind::RuntimeCommand(
                    $profile::RuntimeCommand {
                        fence: Some(fence()),
                        request: 1,
                        action: Some($profile::runtime_command::Action::Key(key)),
                    },
                )),
            });
            assert_eq!(
                Payload::decode(value.major(), &value.encode().unwrap()),
                Ok(value)
            );
        }};
    }
    profile!(v1, Major1);
    profile!(v2, Major2);
}

#[test]
fn wrap_head_spacers_are_distinct_from_wide_tails_and_must_end_a_wrapped_row() {
    let mut page = page1(1, 1);
    let Some(v1::message::Kind::SemanticPage(content)) = &mut page.kind else {
        panic!()
    };
    content.rows[0].soft_wrap = true;
    content.rows[0].cells.push(v1::StyledCell {
        spacer: true,
        wrap_head: true,
        style: Some(style1()),
        ..Default::default()
    });
    assert!(Payload::decode(1, &page.encode_to_vec()).is_ok());
    let Some(v1::message::Kind::SemanticPage(content)) = &mut page.kind else {
        panic!()
    };
    content.rows[0].soft_wrap = false;
    assert_eq!(
        Payload::decode(1, &page.encode_to_vec()),
        Err(ProtocolError::Malformed)
    );
    let mut page = page2(1, 1);
    let Some(v2::message::Kind::SemanticPage(content)) = &mut page.kind else {
        panic!()
    };
    content.rows[0].runs[0].graphemes[0].width = 2;
    assert_eq!(
        Payload::decode(2, &page.encode_to_vec()),
        Err(ProtocolError::Malformed)
    );
}

#[test]
fn a_long_complete_grapheme_is_preserved_within_the_semantic_frame_budget() {
    let mut page = page2(1, 1);
    let Some(v2::message::Kind::SemanticPage(content)) = &mut page.kind else {
        panic!()
    };
    content.rows[0].runs[0].graphemes[0].text = format!("a{}", "\u{301}".repeat(512));
    let expected = Payload::Major2(page.clone());
    assert_eq!(Payload::decode(2, &page.encode_to_vec()), Ok(expected));
}

#[test]
fn new_status_envelopes_require_authenticated_capability_in_both_majors() {
    let fixture = Fixture::new();
    for major in [1, 2] {
        let (mut client, _) = fixture.connect(&[major]);
        let payload = match major {
            1 => Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::StatusQuery(v1::StatusQuery {
                    request: 1,
                    ..Default::default()
                })),
            }),
            2 => Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::StatusQuery(v2::StatusQuery {
                    request: 1,
                    ..Default::default()
                })),
            }),
            _ => unreachable!(),
        };
        assert_eq!(client.encode(&payload), Err(ProtocolError::Negotiation));
    }
}

#[test]
fn remote_policy_ninth_endpoint_is_rejected_by_both_descriptor_profiles() {
    macro_rules! check {
        ($profile:ident, $major:ident) => {{
            let payload = Payload::$major($profile::Message {
                kind: Some($profile::message::Kind::Operation($profile::Operation {
                    id: Some(operation_id()),
                    action: Some($profile::operation::Action::SetRemoteAccess(
                        $profile::SetRemoteAccess {
                            server: Some(support::id(1)),
                            expected_version: 1,
                            value: Some($profile::RemoteAccessPolicy {
                                server: Some(support::id(1)),
                                version: 2,
                                endpoints: (1..=9)
                                    .map(|port| $profile::RemoteEndpoint {
                                        address: vec![127, 0, 0, 1],
                                        port,
                                        ..Default::default()
                                    })
                                    .collect(),
                            }),
                        },
                    )),
                })),
            });
            assert_eq!(payload.encode(), Err(ProtocolError::LimitExceeded));
        }};
    }
    check!(v1, Major1);
    check!(v2, Major2);
}

#[test]
fn standalone_pairing_receipt_body_cannot_exceed_its_reserved_signature_headroom() {
    for major in [1, 2] {
        let proof = spaceterm_protocol::wire::pairing::SignedReceipt {
            canonical_body: vec![1; 1985],
            signature: vec![1; 64],
        };
        let payload = match major {
            1 => Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::PairingOutcome(v1::PairingOutcome {
                    request: 1,
                    enrollment_receipt: Some(Box::new(proof)),
                    ..Default::default()
                })),
            }),
            2 => Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::PairingOutcome(v2::PairingOutcome {
                    request: 1,
                    enrollment_receipt: Some(Box::new(proof)),
                    ..Default::default()
                })),
            }),
            _ => unreachable!(),
        };
        assert_eq!(payload.encode(), Err(ProtocolError::LimitExceeded));
    }
}

fn canonical_physical_page(major: u32, count: usize) -> Payload {
    macro_rules! make {
        ($profile:ident, $variant:ident, $content:expr) => {{
            let page = $profile::CanonicalSemanticPage {
                stamp: Some($profile::QueryStamp {
                    fence: Some(fence()),
                    content_revision: 1,
                    screen: 1,
                    request: 1,
                    request_generation: 0,
                    ..Default::default()
                }),
                geometry: Some($profile::Geometry {
                    rows: 2,
                    columns: 1,
                    cell_width_px: 8,
                    cell_height_px: 16,
                }),
                rows: (0..count)
                    .map(|index| {
                        let mut row = $content;
                        row.id = Some($profile::PhysicalRowId {
                            screen_generation: 0,
                            screen: 1,
                            node_serial: u64::MAX,
                            page_row: index as u32,
                        });
                        row
                    })
                    .collect(),
                complete: true,
                ..Default::default()
            };
            Payload::$variant($profile::Message {
                kind: Some($profile::message::Kind::CanonicalSemanticPage(Box::new(
                    page,
                ))),
            })
        }};
    }
    match major {
        1 => make!(
            v1,
            Major1,
            v1::CanonicalSemanticRow {
                columns: 1,
                end_column: 1,
                cells: vec![v1::StyledCell {
                    grapheme: "x".into(),
                    width: 1,
                    style: Some(style1()),
                    ..Default::default()
                }],
                ..Default::default()
            }
        ),
        2 => make!(
            v2,
            Major2,
            v2::CanonicalSemanticRow {
                columns: 1,
                end_column: 1,
                runs: vec![v2::StyledRun {
                    style: Some(style2()),
                    graphemes: vec![v2::Grapheme {
                        text: "x".into(),
                        width: 1,
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }
        ),
        _ => unreachable!(),
    }
}

#[test]
fn canonical_physical_rows_preserve_shared_node_serial_distinct_page_rows_and_zero_comparisons() {
    for major in [1, 2] {
        let payload = canonical_physical_page(major, 2);
        let bytes = payload.encode().unwrap();
        assert_eq!(Payload::decode(major, &bytes).unwrap(), payload);
    }
}

#[test]
fn canonical_physical_page_row_budget_rejects_limit_plus_one_in_both_majors() {
    for major in [1, 2] {
        assert_eq!(
            canonical_physical_page(major, 257).encode(),
            Err(ProtocolError::LimitExceeded)
        );
    }
}

fn machine_fact_connections(
    fixture: &Fixture,
    major: u32,
    client_facts: bool,
    server_facts: bool,
) -> (
    spaceterm_protocol::connection::AuthenticatedConnection,
    spaceterm_protocol::connection::AuthenticatedConnection,
) {
    machine_fact_connections_with_features(fixture, major, client_facts, server_facts, &[])
}

fn machine_fact_connections_with_features(
    fixture: &Fixture,
    major: u32,
    client_facts: bool,
    server_facts: bool,
    independent_features: &[u32],
) -> (
    spaceterm_protocol::connection::AuthenticatedConnection,
    spaceterm_protocol::connection::AuthenticatedConnection,
) {
    use spaceterm_protocol::{authentication::*, negotiation::*};
    let config = |facts| {
        let mut capabilities = vec![CAPABILITY_CANONICAL_DOMAIN];
        capabilities.extend_from_slice(independent_features);
        if facts {
            capabilities.push(CAPABILITY_SERVER_MACHINE_FACTS);
        }
        HandshakeConfig {
            majors: vec![major],
            capabilities,
            ..Default::default()
        }
    };
    let (client, hello) = ClientHandshake::begin(
        fixture.pin,
        fixture.principal,
        Purpose::Ordinary,
        config(client_facts),
        fixture.device_key.clone(),
    )
    .unwrap();
    let (server, reply) = ServerHandshake::accept(
        fixture.principal.server,
        7,
        config(server_facts),
        fixture.server_key.clone(),
        &fixture.pairings,
        &hello,
    )
    .unwrap();
    let (client, proof) = client.receive_server(&reply).unwrap();
    let (Established::Ordinary(server), ready) =
        server.receive_device(&fixture.pairings, &proof).unwrap()
    else {
        panic!("ordinary server")
    };
    let Established::Ordinary(client) = client.receive_ready(&ready).unwrap() else {
        panic!("ordinary client")
    };
    (client, server)
}

fn machine_fact_payload(major: u32, publication: bool, facts: bool) -> Payload {
    macro_rules! profile {
        ($p:ident, $variant:ident) => {{
            let status = $p::ManagementStatus {
                request: if publication { 0 } else { 1 },
                state: 1,
                epoch: 7,
                server: Some(support::id(1)),
                status_revision: 1,
                remote_access: Some($p::RemoteAccessPolicy {
                    server: Some(support::id(1)),
                    version: 1,
                    ..Default::default()
                }),
                machine_facts: facts.then($p::ServerMachineFacts::default),
                ..Default::default()
            };
            Payload::$variant($p::Message {
                kind: Some(if publication {
                    $p::message::Kind::StatusPublication($p::StatusPublication {
                        request: 1,
                        request_generation: 1,
                        status: Some(status),
                        ..Default::default()
                    })
                } else {
                    $p::message::Kind::ManagementStatus(status)
                }),
            })
        }};
    }
    match major {
        1 => profile!(v1, Major1),
        2 => profile!(v2, Major2),
        _ => unreachable!(),
    }
}

#[test]
fn machine_facts_require_both_advertisements_and_sender_receiver_gates_in_both_majors() {
    use spaceterm_protocol::{
        authentication::SigningKey, negotiation::CAPABILITY_SERVER_MACHINE_FACTS,
    };
    let fixture = Fixture::new();
    for major in [1, 2] {
        for publication in [false, true] {
            for (client_facts, server_facts) in
                [(false, false), (true, false), (false, true), (true, true)]
            {
                let (mut client, mut server) =
                    machine_fact_connections(&fixture, major, client_facts, server_facts);
                let supported = client_facts && server_facts;
                assert_eq!(
                    client
                        .capabilities()
                        .contains(&CAPABILITY_SERVER_MACHINE_FACTS),
                    supported
                );
                let facts = machine_fact_payload(major, publication, true);
                if supported {
                    assert_eq!(client.decode(&server.encode(&facts).unwrap()), Ok(facts));
                    continue;
                }
                assert_eq!(server.encode(&facts), Err(ProtocolError::Negotiation));
                // An outgoing rejection must not consume the first sequence.
                let omitted = machine_fact_payload(major, publication, false);
                assert_eq!(
                    client.decode(&server.encode(&omitted).unwrap()),
                    Ok(omitted)
                );
                // A paired sender can sign a structurally valid but unnegotiated
                // field. Verify the receive gate after genuine signature checks.
                let body = common::AuthenticatedEnvelope {
                    direction: common::Direction::ServerToClient as i32,
                    major,
                    binding: client.binding().to_vec(),
                    sequence: 2,
                    payload: facts.encode().unwrap(),
                    purpose: 1,
                }
                .encode_to_vec();
                let mut transcript = Vec::new();
                for component in [
                    b"SpaceTerm Protocol Ed25519 v1".as_slice(),
                    b"authenticated-envelope".as_slice(),
                    body.as_slice(),
                ] {
                    transcript.extend_from_slice(&(component.len() as u64).to_be_bytes());
                    transcript.extend_from_slice(component);
                }
                let bytes = common::Signed {
                    signature: fixture.server_key.sign(&transcript).unwrap().to_vec(),
                    body,
                }
                .encode_to_vec();
                assert_eq!(client.decode(&bytes), Err(ProtocolError::Negotiation));
                assert_eq!(client.decode(&bytes), Err(ProtocolError::Authentication));
            }
        }
    }
}

#[test]
fn machine_facts_are_independent_of_reserved_metadata_paste_and_copy_features_in_both_majors() {
    use spaceterm_protocol::negotiation::CAPABILITY_SERVER_MACHINE_FACTS;
    // The integrated feature namespace is reserved even before this lane receives its converters.
    let reserved = [520_010, 520_011, 520_012];
    let fixture = Fixture::new();
    for major in [1, 2] {
        for independent in [
            &[][..],
            &reserved[..1],
            &reserved[1..2],
            &reserved[2..],
            &reserved[..],
        ] {
            for facts_supported in [false, true] {
                for publication in [false, true] {
                    let (mut client, mut server) = machine_fact_connections_with_features(
                        &fixture,
                        major,
                        facts_supported,
                        facts_supported,
                        independent,
                    );
                    assert_eq!(
                        client
                            .capabilities()
                            .contains(&CAPABILITY_SERVER_MACHINE_FACTS),
                        facts_supported
                    );
                    let facts = machine_fact_payload(major, publication, true);
                    if facts_supported {
                        assert_eq!(client.decode(&server.encode(&facts).unwrap()), Ok(facts));
                    } else {
                        assert_eq!(server.encode(&facts), Err(ProtocolError::Negotiation));
                        let omitted = machine_fact_payload(major, publication, false);
                        assert_eq!(
                            client.decode(&server.encode(&omitted).unwrap()),
                            Ok(omitted)
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn machine_fact_scope_and_endpoint_limits_apply_before_decode_in_both_majors() {
    macro_rules! check {
        ($p:ident, $variant:ident, $major:expr) => {{
            for (scopes, endpoints) in [(3, 2), (1, 9)] {
                let payload = $p::Message {
                    kind: Some($p::message::Kind::ManagementStatus($p::ManagementStatus {
                        state: 1,
                        machine_facts: Some($p::ServerMachineFacts {
                            bind_scope_availability: 1,
                            bind_scopes: vec![
                                $p::ServerBindScopeOffer {
                                    scope: 1,
                                    endpoints: vec![
                                        $p::RemoteEndpoint {
                                            address: vec![127, 0, 0, 1],
                                            port: 47230,
                                            ..Default::default()
                                        };
                                        endpoints
                                    ],
                                };
                                scopes
                            ],
                            ..Default::default()
                        }),
                        ..Default::default()
                    })),
                };
                assert_eq!(
                    Payload::$variant(payload.clone()).encode(),
                    Err(ProtocolError::LimitExceeded)
                );
                assert_eq!(
                    Payload::decode($major, &payload.encode_to_vec()),
                    Err(ProtocolError::LimitExceeded)
                );
            }
        }};
    }
    check!(v1, Major1, 1);
    check!(v2, Major2, 2);
}
