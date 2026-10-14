use spaceterm_core::{authority::*, content::*, content_queries::*, identity::*};
use spaceterm_protocol::{
    ProtocolError,
    codec::Payload,
    convert::{content, identity, runtime},
    wire::{v1, v2},
};
use spaceterm_terminal::{KeyAction, KeyInput, PhysicalKey, runtime_contracts::*};

fn fence(content: Option<ContentRevision>) -> EffectFence {
    let server = ServerId::new([1; 16]);
    let pane = PaneId::new(server, 1).unwrap();
    let client = ClientId::new(server, 1).unwrap();
    EffectFence::new(
        SessionKey {
            pane,
            epoch: ServerEpoch::new(1),
            generation: SessionGeneration::new(1),
        },
        ControlGrant::new(pane, client, ControlGeneration::new(2)).unwrap(),
        ConnectionBinding::new(client, ConnectionGeneration::new(3)),
        ContextGeneration::new(4),
        content,
    )
    .unwrap()
}
fn page() -> (ContentPageRequest, ContentPageReply) {
    let fence = fence(Some(ContentRevision::new(0)));
    let base = ContentBase {
        session: fence.session(),
        revision: ContentRevision::new(0),
        topology_revision: 0,
        screen_generation: 0,
        screen: Screen::Primary,
    };
    let stamp = QueryStamp::new(
        fence,
        base,
        ContentRequestId::new(1).unwrap(),
        RequestGeneration::new(2),
    )
    .unwrap();
    let request = ContentPageRequest::new(
        stamp,
        ContentRange::ActiveGrid,
        None,
        PageLimits::new(256, 16384, 1024 * 1024).unwrap(),
    )
    .unwrap();
    let row = RowId {
        screen: Screen::Primary,
        screen_generation: 0,
        node_serial: 0,
        page_row: 0,
    };
    let style = CellStyle {
        foreground: TerminalColor::Rgb(Rgb { r: 0, g: 0, b: 0 }),
        background: TerminalColor::Palette(255),
        underline_color: TerminalColor::Rgb(Rgb { r: 1, g: 2, b: 3 }),
        underline: Underline::Curly,
        bold: true,
        faint: true,
        italic: true,
        blinking: true,
        inverse: true,
        invisible: true,
        strikethrough: true,
        overline: true,
        semantic: SemanticContent::Prompt,
    };
    let link = HyperlinkId::new(1).unwrap();
    let cells = vec![
        CellContent::grapheme("A".into(), CellWidth::Single).unwrap(),
        CellContent::grapheme("🦀".into(), CellWidth::Wide).unwrap(),
        CellContent::spacer_tail(),
        CellContent::grapheme("e\u{301}".into(), CellWidth::Single).unwrap(),
    ];
    let mut parts = ContentPageParts::new(base, 4);
    parts.rows.push(
        ContentRow::new(
            row,
            0,
            false,
            cells
                .into_iter()
                .map(|cell| StyledCell::new(cell, style, Some(link)))
                .collect(),
        )
        .unwrap(),
    );
    parts
        .hyperlinks
        .push(HyperlinkDescriptor::new(link, "https://example.invalid/untrusted".into()).unwrap());
    parts.cursor = Some(Cursor {
        row,
        column: 3,
        shape: CursorShape::Bar,
        visible: true,
        blinking: true,
    });
    parts.colors = ProgramColorOverrides {
        foreground: Some(Rgb { r: 0, g: 0, b: 0 }),
        background: None,
        cursor: Some(Rgb { r: 4, g: 5, b: 6 }),
        palette: vec![PaletteOverride {
            index: 255,
            color: Rgb { r: 7, g: 8, b: 9 },
        }],
    };
    parts.modes = TerminalModes {
        application_cursor: true,
        application_keypad: true,
        bracketed_paste: true,
        focus_reporting: true,
        mouse_tracking: MouseTracking::Any,
        mouse_encoding: MouseEncoding::SgrPixels,
        synchronized_output: true,
        origin: true,
        insert: true,
        wraparound: true,
        reverse_colors: true,
        hidden_input: true,
    };
    let reply = ContentPageReply::new(&request, ContentPage::new(parts).unwrap()).unwrap();
    (request, reply)
}
macro_rules! cases {
    ($name:ident, $p:ident, $major:ident) => {
        #[test]
        fn $name() {
            let (request, reply) = page();
            let encoded = content::$p::encode_page(&reply).unwrap();
            let bytes = Payload::$major($p::Message {
                kind: Some($p::message::Kind::CanonicalSemanticPage(Box::new(encoded))),
            })
            .encode()
            .unwrap();
            let Payload::$major($p::Message {
                kind: Some($p::message::Kind::CanonicalSemanticPage(encoded)),
            }) = Payload::decode(
                match stringify!($p) {
                    "v1" => 1,
                    _ => 2,
                },
                &bytes,
            )
            .unwrap()
            else {
                panic!("canonical page")
            };
            assert_eq!(content::$p::decode_page(&encoded, &request).unwrap(), reply);
            let mut changed = encoded.clone();
            changed.stamp.as_mut().unwrap().request_generation += 1;
            assert_eq!(
                content::$p::decode_page(&changed, &request),
                Err(ProtocolError::Malformed)
            );
            let mut changed = encoded;
            changed.rows[0].end_column -= 1;
            assert_eq!(
                content::$p::decode_page(&changed, &request),
                Err(ProtocolError::Malformed)
            );
            assert_eq!(
                content::$p::decode_page_request(
                    &content::$p::encode_page_request(request).unwrap()
                )
                .unwrap(),
                request
            );
        }
    };
}
cases!(
    semantic_rows_preserve_zero_comparisons_styles_modes_and_cursor_major_1,
    v1,
    Major1
);
cases!(
    semantic_rows_preserve_zero_comparisons_styles_modes_and_cursor_major_2,
    v2,
    Major2
);

macro_rules! inputs {
    ($name:ident, $p:ident, $major:ident) => {
        #[test]
        fn $name() {
            let command = FencedRuntimeCommand {
                fence: fence(None),
                command: RuntimeCommand::RequestPaste(
                    BoundedPaste::new("x".repeat(1024 * 1024)).unwrap(),
                ),
            };
            let wire = runtime::$p::encode_command(1, &command).unwrap();
            assert_eq!(runtime::$p::decode_command(&wire).unwrap(), command);
            assert!(
                Payload::$major($p::Message {
                    kind: Some($p::message::Kind::RuntimeCommand(wire))
                })
                .encode()
                .is_ok()
            );
            let too_large = $p::RuntimeCommand {
                request: 1,
                fence: Some(identity::encode_fence(fence(None))),
                action: Some($p::runtime_command::Action::Paste($p::Paste {
                    text: vec![b'x'; 1024 * 1024 + 1],
                    approval: vec![],
                })),
            };
            assert_eq!(
                runtime::$p::decode_command(&too_large),
                Err(ProtocolError::LimitExceeded)
            );
            assert_eq!(
                Payload::$major($p::Message {
                    kind: Some($p::message::Kind::RuntimeCommand(too_large))
                })
                .encode(),
                Err(ProtocolError::LimitExceeded)
            );
            let mut key = KeyInput::text_input("é");
            key.action = KeyAction::Repeat;
            key.physical_key = PhysicalKey::E;
            key.unshifted_codepoint = Some('e');
            key.modifiers.alt_right = true;
            key.modifiers.control = true;
            key.consumed_modifiers.shift = true;
            let command = FencedRuntimeCommand {
                fence: fence(None),
                command: RuntimeCommand::Key(key),
            };
            assert_eq!(
                runtime::$p::decode_command(&runtime::$p::encode_command(2, &command).unwrap())
                    .unwrap(),
                command
            );
        }
    };
}
inputs!(
    portable_keys_and_exact_one_mib_paste_limit_major_1,
    v1,
    Major1
);
inputs!(
    portable_keys_and_exact_one_mib_paste_limit_major_2,
    v2,
    Major2
);

#[test]
fn both_major_content_page_requests_round_trip_through_authenticated_payloads() {
    use spaceterm_protocol::authentication::{
        ClientHandshake, Established, HandshakeConfig, PairingRecord, Principal, Purpose,
        ServerHandshake, ServerPin, SigningKey,
    };
    #[expect(
        dead_code,
        reason = "Only signing and pairing helpers from the shared fixture are needed"
    )]
    #[path = "support/mod.rs"]
    mod auth_support;
    for major in [1, 2] {
        let server_key = auth_support::TestKey::seeded(41);
        let device_key = auth_support::TestKey::seeded(42);
        let mut client_id = [0; 16];
        client_id[8..].copy_from_slice(&1u64.to_be_bytes());
        let principal = Principal {
            server: [1; 16],
            device: [2; 16],
            pairing: [3; 16],
            client: client_id,
        };
        let pairings = auth_support::Pairings {
            record: PairingRecord {
                principal,
                device_public_key: device_key.public_key(),
                active: true,
            },
            active: std::cell::Cell::new(true),
        };
        let config = HandshakeConfig {
            majors: vec![major],
            capabilities: vec![spaceterm_protocol::negotiation::CAPABILITY_CANONICAL_DOMAIN],
            ..Default::default()
        };
        let (client, hello) = ClientHandshake::begin(
            ServerPin {
                identity: principal.server,
                public_key: server_key.public_key(),
            },
            principal,
            Purpose::Ordinary,
            config.clone(),
            device_key,
        )
        .unwrap();
        let (server, reply) =
            ServerHandshake::accept(principal.server, 1, config, server_key, &pairings, &hello)
                .unwrap();
        let (client, proof) = client.receive_server(&reply).unwrap();
        let (server, ready) = server.receive_device(&pairings, &proof).unwrap();
        let Established::Ordinary(mut client) = client.receive_ready(&ready).unwrap() else {
            panic!("ordinary client")
        };
        let Established::Ordinary(mut server) = server else {
            panic!("ordinary server")
        };
        let (active, _) = page();
        for range in [
            ContentRange::ActiveGrid,
            ContentRange::History {
                first: None,
                last: None,
            },
        ] {
            let request =
                ContentPageRequest::new(active.stamp(), range, None, active.limits()).unwrap();
            let payload = if major == 1 {
                Payload::Major1(v1::Message {
                    kind: Some(v1::message::Kind::ContentQuery(
                        content::v1::encode_page_request(request).unwrap(),
                    )),
                })
            } else {
                Payload::Major2(v2::Message {
                    kind: Some(v2::message::Kind::ContentQuery(
                        content::v2::encode_page_request(request).unwrap(),
                    )),
                })
            };
            let bytes = payload
                .encode()
                .expect("canonical request must pass required-field preflight");
            assert_eq!(Payload::decode(major, &bytes).unwrap(), payload);
            let encoded = client.encode(&payload).unwrap();
            assert_eq!(server.decode(&encoded).unwrap(), payload);
        }
    }
}
macro_rules! mouse_inputs {
    ($name:ident, $p:ident) => {
        #[test]
        fn $name() {
            use spaceterm_terminal::{
                InputModifiers,
                pointer_input::{PointerButton, PointerPhase},
            };
            let captured = fence(Some(ContentRevision::new(7)));
            let base = ContentBase {
                session: captured.session(),
                revision: ContentRevision::new(7),
                topology_revision: 8,
                screen_generation: 9,
                screen: Screen::Alternate,
            };
            let stamp = QueryStamp::new(
                captured,
                base,
                ContentRequestId::new(11).unwrap(),
                RequestGeneration::new(12),
            )
            .unwrap();
            let cell = CellRef {
                session: base.session,
                row: RowId {
                    screen: base.screen,
                    screen_generation: 9,
                    node_serial: 0,
                    page_row: 13,
                },
                row_revision: 14,
                column: 15,
            };
            let modifiers = InputModifiers {
                shift: true,
                control: true,
                alt_right: true,
                ..Default::default()
            };
            for command in [
                RuntimeCommand::Pointer(ApplicationPointerInput {
                    stamp,
                    cell,
                    phase: PointerPhase::Press,
                    button: Some(PointerButton::Left),
                    modifiers,
                }),
                RuntimeCommand::Pointer(ApplicationPointerInput {
                    stamp,
                    cell,
                    phase: PointerPhase::Motion,
                    button: Some(PointerButton::Right),
                    modifiers,
                }),
                RuntimeCommand::Pointer(ApplicationPointerInput {
                    stamp,
                    cell,
                    phase: PointerPhase::Release,
                    button: Some(PointerButton::Middle),
                    modifiers,
                }),
                RuntimeCommand::Pointer(ApplicationPointerInput {
                    stamp,
                    cell,
                    phase: PointerPhase::Motion,
                    button: None,
                    modifiers,
                }),
                RuntimeCommand::Wheel(ApplicationWheelInput {
                    stamp,
                    cell,
                    horizontal_steps: -3,
                    vertical_steps: 4,
                    modifiers,
                }),
            ] {
                let expected = FencedRuntimeCommand {
                    fence: captured,
                    command,
                };
                let encoded = runtime::$p::encode_command(21, &expected)
                    .expect("application mouse input must encode");
                assert_eq!(runtime::$p::decode_command(&encoded).unwrap(), expected);
                let mut wrong = expected.clone();
                wrong.fence = fence(Some(ContentRevision::new(6)));
                assert_eq!(
                    runtime::$p::encode_command(21, &wrong),
                    Err(ProtocolError::Malformed)
                );
            }
        }
    };
}
mouse_inputs!(
    application_mouse_preserves_complete_content_binding_major_1,
    v1
);
mouse_inputs!(
    application_mouse_preserves_complete_content_binding_major_2,
    v2
);
