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
