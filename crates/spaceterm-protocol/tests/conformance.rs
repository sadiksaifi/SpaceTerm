mod support;

use prost::Message;
use spaceterm_protocol::{
    ProtocolError,
    authentication::{
        ClientHandshake, Established, HandshakeConfig, PairingRecord, Purpose, ServerHandshake,
    },
    codec::Payload,
    frame::{FrameDecoder, encode_frame},
    wire::{common, v1, v2},
};
use support::{Fixture, Pairings, TestKey, entity, fence, id, operation_id};

fn create(major: u32) -> Payload {
    match major {
        1 => Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::Operation(v1::Operation {
                id: Some(operation_id()),
                action: Some(v1::operation::Action::CreatePane(v1::CreatePane {
                    tab: Some(entity(2)),
                    source: Some(entity(3)),
                    edge: v1::Edge::Right as i32,
                })),
            })),
        }),
        2 => Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::Operation(v2::Operation {
                id: Some(operation_id()),
                action: Some(v2::operation::Action::CreatePane(v2::CreatePane {
                    tab: Some(entity(2)),
                    source: Some(entity(3)),
                    edge: v2::Edge::Right as i32,
                })),
            })),
        }),
        _ => panic!("test major"),
    }
}
fn outcome(major: u32) -> Payload {
    match major {
        1 => Payload::Major1(v1::Message {
            kind: Some(v1::message::Kind::Outcome(v1::Outcome {
                operation: Some(operation_id()),
                publication_revision: 8,
                result: Some(v1::outcome::Result::Created(entity(4))),
            })),
        }),
        2 => Payload::Major2(v2::Message {
            kind: Some(v2::message::Kind::Outcome(v2::Outcome {
                operation: Some(operation_id()),
                publication_revision: 8,
                result: Some(v2::outcome::Result::Created(entity(4))),
            })),
        }),
        _ => panic!("test major"),
    }
}
fn styled_page(major: u32) -> Payload {
    match major {
        1 => {
            let style = v1::Style {
                foreground: Some(v1::Color {
                    value: Some(v1::color::Value::Rgb(0xff6600)),
                }),
                background: Some(v1::Color {
                    value: Some(v1::color::Value::DefaultColor(true)),
                }),
                flags: 3,
                ..Default::default()
            };
            Payload::Major1(v1::Message {
                kind: Some(v1::message::Kind::SemanticPage(v1::SemanticPage {
                    metadata: Some(v1::TerminalMetadata {
                        title: "editor".into(),
                        hidden_input: true,
                        version: 1,
                        ..Default::default()
                    }),
                    geometry: Some(v1::Geometry {
                        rows: 1,
                        columns: 3,
                        cell_width_px: 8,
                        cell_height_px: 16,
                    }),
                    fence: Some(fence()),
                    request_generation: 1,
                    content_revision: 9,
                    screen_generation: 1,
                    topology_revision: 1,
                    rows: vec![v1::SemanticRow {
                        id: 10,
                        revision: 9,
                        soft_wrap: true,
                        cells: vec![
                            v1::StyledCell {
                                grapheme: "A\u{301}".into(),
                                width: 1,
                                style: Some(style),
                                hyperlink_id: 1,
                                spacer: false,
                                wrap_head: false,
                            },
                            v1::StyledCell {
                                grapheme: "界".into(),
                                width: 2,
                                style: Some(style),
                                hyperlink_id: 1,
                                spacer: false,
                                wrap_head: false,
                            },
                            v1::StyledCell {
                                spacer: true,
                                style: Some(style),
                                hyperlink_id: 1,
                                ..Default::default()
                            },
                        ],
                        physical_row: None,
                        start_column: 0,
                        end_column: 0,
                        columns: 0,
                    }],
                    color_state: Some(v1::ColorState {
                        default_foreground: Some(0x01_0203),
                        default_background: Some(0),
                        cursor_color: Some(0x04_0506),
                        reverse_colors: true,
                    }),
                    color_overrides: vec![v1::ColorOverride {
                        slot: 1,
                        rgb: 0x0a_0b0c,
                    }],
                    hyperlinks: vec![v1::Hyperlink {
                        id: 1,
                        uri: "https://example.org".into(),
                    }],
                    cursor: Some(v1::Cursor {
                        row_id: 10,
                        column: 2,
                        shape: 1,
                        visible: true,
                        blinking: false,
                        physical_row: None,
                    }),
                    modes: Some(v1::Modes {
                        bracketed_paste: true,
                        ..Default::default()
                    }),
                    complete: true,
                    ..Default::default()
                })),
            })
        }
        2 => {
            let style = v2::Style {
                foreground: Some(v2::Color {
                    value: Some(v2::color::Value::Rgb(0xff6600)),
                }),
                background: Some(v2::Color {
                    value: Some(v2::color::Value::DefaultColor(true)),
                }),
                flags: 3,
                ..Default::default()
            };
            Payload::Major2(v2::Message {
                kind: Some(v2::message::Kind::SemanticPage(v2::SemanticPage {
                    metadata: Some(v2::TerminalMetadata {
                        title: "editor".into(),
                        hidden_input: true,
                        version: 1,
                        ..Default::default()
                    }),
                    geometry: Some(v2::Geometry {
                        rows: 1,
                        columns: 3,
                        cell_width_px: 8,
                        cell_height_px: 16,
                    }),
                    fence: Some(fence()),
                    request_generation: 1,
                    content_revision: 9,
                    screen_generation: 1,
                    topology_revision: 1,
                    rows: vec![v2::SemanticRow {
                        id: 10,
                        revision: 9,
                        soft_wrap: true,
                        runs: vec![v2::StyledRun {
                            style: Some(style),
                            hyperlink_id: 1,
                            graphemes: vec![
                                v2::Grapheme {
                                    text: "A\u{301}".into(),
                                    width: 1,
                                    spacer: false,
                                    wrap_head: false,
                                },
                                v2::Grapheme {
                                    text: "界".into(),
                                    width: 2,
                                    spacer: false,
                                    wrap_head: false,
                                },
                                v2::Grapheme {
                                    spacer: true,
                                    ..Default::default()
                                },
                            ],
                        }],
                        physical_row: None,
                        start_column: 0,
                        end_column: 0,
                        columns: 0,
                    }],
                    color_state: Some(v2::ColorState {
                        default_foreground: Some(0x01_0203),
                        default_background: Some(0),
                        cursor_color: Some(0x04_0506),
                        reverse_colors: true,
                    }),
                    color_overrides: vec![v2::ColorOverride {
                        slot: 1,
                        rgb: 0x0a_0b0c,
                    }],
                    hyperlinks: vec![v2::Hyperlink {
                        id: 1,
                        uri: "https://example.org".into(),
                    }],
                    cursor: Some(v2::Cursor {
                        row_id: 10,
                        column: 2,
                        shape: 1,
                        visible: true,
                        blinking: false,
                        physical_row: None,
                    }),
                    modes: Some(v2::Modes {
                        bracketed_paste: true,
                        ..Default::default()
                    }),
                    complete: true,
                    ..Default::default()
                })),
            })
        }
        _ => panic!("test major"),
    }
}
fn fragmented(bytes: &[u8]) -> Vec<u8> {
    let framed = encode_frame(bytes).unwrap();
    let mut decoder = FrameDecoder::new();
    let mut decoded = None;
    for chunk in framed.chunks(3) {
        let (consumed, frame) = decoder.push(chunk).unwrap();
        assert_eq!(consumed, chunk.len());
        if frame.is_some() {
            assert!(decoded.is_none());
            decoded = frame;
        }
    }
    decoded.unwrap()
}

#[test]
fn current_and_previous_profiles_authenticate_and_round_trip_create_outcome_and_styled_content() {
    let fixture = Fixture::new();
    for (majors, expected) in [(&[2, 1][..], 2), (&[1][..], 1)] {
        let (mut client, mut server) = fixture.connect(majors);
        assert_eq!(client.major(), expected);
        assert_eq!(server.major(), expected);
        assert_eq!(client.principal(), fixture.principal);
        assert_eq!(client.epoch(), 7);
        let request = create(expected);
        let encoded = client.encode(&request).unwrap();
        assert_eq!(server.decode(&fragmented(&encoded)), Ok(request));
        for response in [outcome(expected), styled_page(expected)] {
            let encoded = server.encode(&response).unwrap();
            assert_eq!(client.decode(&fragmented(&encoded)), Ok(response));
        }
    }
}

#[test]
fn server_pin_and_device_key_are_verified_independently() {
    let fixture = Fixture::new();
    let mut pin = fixture.pin;
    pin.public_key = [9; 32];
    let (client, hello) = ClientHandshake::begin(
        pin,
        fixture.principal,
        Purpose::Ordinary,
        HandshakeConfig::default(),
        fixture.device_key.clone(),
    )
    .unwrap();
    let (_, reply) = ServerHandshake::accept(
        fixture.principal.server,
        7,
        HandshakeConfig::default(),
        fixture.server_key.clone(),
        &fixture.pairings,
        &hello,
    )
    .unwrap();
    assert!(matches!(
        client.receive_server(&reply),
        Err(ProtocolError::Authentication)
    ));

    let (client, hello) = ClientHandshake::begin(
        fixture.pin,
        fixture.principal,
        Purpose::Ordinary,
        HandshakeConfig::default(),
        TestKey::seeded(99),
    )
    .unwrap();
    let (server, reply) = ServerHandshake::accept(
        fixture.principal.server,
        7,
        HandshakeConfig::default(),
        fixture.server_key.clone(),
        &fixture.pairings,
        &hello,
    )
    .unwrap();
    let (_, proof) = client.receive_server(&reply).unwrap();
    assert!(matches!(
        server.receive_device(&fixture.pairings, &proof),
        Err(ProtocolError::Authentication)
    ));
    assert!(matches!(
        ServerHandshake::accept(
            [99; 16],
            7,
            HandshakeConfig::default(),
            fixture.server_key.clone(),
            &fixture.pairings,
            &hello
        ),
        Err(ProtocolError::Authentication)
    ));
}

#[test]
fn substituted_advertisements_selection_and_exact_transcript_encoding_are_rejected() {
    let fixture = Fixture::new();
    for changed_selection in [false, true] {
        let (client, hello) = ClientHandshake::begin(
            fixture.pin,
            fixture.principal,
            Purpose::Ordinary,
            HandshakeConfig::default(),
            fixture.device_key.clone(),
        )
        .unwrap();
        let (_, reply) = ServerHandshake::accept(
            fixture.principal.server,
            7,
            HandshakeConfig::default(),
            fixture.server_key.clone(),
            &fixture.pairings,
            &hello,
        )
        .unwrap();
        let mut signed = common::Signed::decode(reply.as_slice()).unwrap();
        let mut body = common::ServerHello::decode(signed.body.as_slice()).unwrap();
        if changed_selection {
            body.selected_major = 1;
        } else {
            body.majors = vec![1];
        }
        signed.body = body.encode_to_vec();
        assert!(matches!(
            client.receive_server(&signed.encode_to_vec()),
            Err(ProtocolError::Authentication)
        ));
    }
    let (client, hello) = ClientHandshake::begin(
        fixture.pin,
        fixture.principal,
        Purpose::Ordinary,
        HandshakeConfig::default(),
        fixture.device_key.clone(),
    )
    .unwrap();
    let mut altered = common::ClientHello::decode(hello.as_slice()).unwrap();
    altered.majors = vec![1];
    let (_, reply) = ServerHandshake::accept(
        fixture.principal.server,
        7,
        HandshakeConfig::default(),
        fixture.server_key.clone(),
        &fixture.pairings,
        &altered.encode_to_vec(),
    )
    .unwrap();
    assert!(matches!(
        client.receive_server(&reply),
        Err(ProtocolError::Authentication)
    ));

    // Move a known field to the beginning without changing its decoded meaning.
    // Decode-and-reencode verification would erase this substitution.
    let (client, hello) = ClientHandshake::begin(
        fixture.pin,
        fixture.principal,
        Purpose::Ordinary,
        HandshakeConfig::default(),
        fixture.device_key.clone(),
    )
    .unwrap();
    assert_eq!(&hello[hello.len() - 2..], &[0x48, 1]);
    let reordered = [&hello[hello.len() - 2..], &hello[..hello.len() - 2]].concat();
    assert_eq!(
        common::ClientHello::decode(reordered.as_slice()).unwrap(),
        common::ClientHello::decode(hello.as_slice()).unwrap()
    );
    let (_, reply) = ServerHandshake::accept(
        fixture.principal.server,
        7,
        HandshakeConfig::default(),
        fixture.server_key.clone(),
        &fixture.pairings,
        &reordered,
    )
    .unwrap();
    assert!(matches!(
        client.receive_server(&reply),
        Err(ProtocolError::Authentication)
    ));
}

#[test]
fn revoked_pairing_or_wrong_client_binding_never_reaches_ordinary_readiness() {
    let fixture = Fixture::new();
    let (client, hello) = ClientHandshake::begin(
        fixture.pin,
        fixture.principal,
        Purpose::Ordinary,
        HandshakeConfig::default(),
        fixture.device_key.clone(),
    )
    .unwrap();
    let (server, reply) = ServerHandshake::accept(
        fixture.principal.server,
        7,
        HandshakeConfig::default(),
        fixture.server_key.clone(),
        &fixture.pairings,
        &hello,
    )
    .unwrap();
    let (_, proof) = client.receive_server(&reply).unwrap();
    fixture.pairings.active.set(false);
    assert!(matches!(
        server.receive_device(&fixture.pairings, &proof),
        Err(ProtocolError::PairingDenied)
    ));
    assert!(matches!(
        ServerHandshake::accept(
            fixture.principal.server,
            7,
            HandshakeConfig::default(),
            fixture.server_key.clone(),
            &fixture.pairings,
            &hello
        ),
        Err(ProtocolError::PairingDenied)
    ));
    let mismatched = Pairings {
        active: std::cell::Cell::new(true),
        record: PairingRecord {
            principal: spaceterm_protocol::authentication::Principal {
                device: [8; 16],
                ..fixture.principal
            },
            ..fixture.pairings.record
        },
    };
    assert!(matches!(
        ServerHandshake::accept(
            fixture.principal.server,
            7,
            HandshakeConfig::default(),
            fixture.server_key.clone(),
            &mismatched,
            &hello
        ),
        Err(ProtocolError::PairingDenied)
    ));
}

#[test]
fn replay_reorder_cross_connection_and_payload_tampering_are_terminal_failures() {
    let fixture = Fixture::new();
    let request = create(2);
    let (mut client, mut server) = fixture.connect(&[2, 1]);
    let envelope = client.encode(&request).unwrap();
    assert_eq!(server.decode(&envelope), Ok(request.clone()));
    assert_eq!(server.decode(&envelope), Err(ProtocolError::Sequence));
    assert_eq!(
        server.decode(&client.encode(&request).unwrap()),
        Err(ProtocolError::Authentication)
    );

    let (mut client, mut server) = fixture.connect(&[2, 1]);
    client.encode(&request).unwrap();
    assert_eq!(
        server.decode(&client.encode(&request).unwrap()),
        Err(ProtocolError::Sequence)
    );

    let (mut client, _) = fixture.connect(&[2, 1]);
    let (_, mut other_server) = fixture.connect(&[2, 1]);
    let envelope = client.encode(&request).unwrap();
    assert_eq!(
        other_server.decode(&envelope),
        Err(ProtocolError::Authentication)
    );

    let (mut client, mut server) = fixture.connect(&[2, 1]);
    let bytes = client.encode(&request).unwrap();
    let mut signed = common::Signed::decode(bytes.as_slice()).unwrap();
    let mut envelope = common::AuthenticatedEnvelope::decode(signed.body.as_slice()).unwrap();
    envelope.payload = outcome(2).encode().unwrap();
    signed.body = envelope.encode_to_vec();
    assert_eq!(
        server.decode(&signed.encode_to_vec()),
        Err(ProtocolError::Authentication)
    );
}

#[test]
fn revoked_credentials_can_only_exchange_original_self_revocation_and_signed_receipts() {
    let fixture = Fixture::new();
    fixture.pairings.active.set(false);
    let (client, server) = fixture.establish(&[2, 1], Purpose::RevocationStatus);
    let Established::Restricted(mut client) = client else {
        panic!("restricted client")
    };
    let Established::Restricted(mut server) = server else {
        panic!("restricted server")
    };
    let request = common::RestrictedMessage {
        kind: Some(common::restricted_message::Kind::Revoke(
            common::RevocationRequest {
                operation: Some(operation_id()),
                target_pairing: Some(id(3)),
            },
        )),
    };
    assert_eq!(
        server.decode(&client.encode(&request).unwrap()),
        Ok(request)
    );
    let receipt = common::RestrictedMessage {
        kind: Some(common::restricted_message::Kind::Receipt(
            common::RevocationReceipt {
                operation: Some(operation_id()),
                target_pairing: Some(id(3)),
                state: common::RevocationState::Revoked as i32,
                store_revision: 9,
            },
        )),
    };
    assert_eq!(
        client.decode(&server.encode(&receipt).unwrap()),
        Ok(receipt)
    );
    let other_pairing = common::RestrictedMessage {
        kind: Some(common::restricted_message::Kind::Status(
            common::RevocationStatus {
                operation: Some(operation_id()),
                target_pairing: Some(id(99)),
            },
        )),
    };
    assert_eq!(
        client.encode(&other_pairing),
        Err(ProtocolError::PairingDenied)
    );
    assert_eq!(
        client.encode(&common::RestrictedMessage {
            kind: Some(common::restricted_message::Kind::Receipt(
                common::RevocationReceipt {
                    operation: Some(operation_id()),
                    target_pairing: Some(id(3)),
                    state: 2,
                    store_revision: 9
                }
            ))
        }),
        Err(ProtocolError::UnexpectedMessage)
    );
}

#[test]
fn capabilities_and_limits_are_bounded_and_negotiated_without_forcing_a_major() {
    let fixture = Fixture::new();
    let config = HandshakeConfig {
        majors: vec![2, 1],
        capabilities: vec![1, 3],
        limits: spaceterm_protocol::limits::NegotiatedLimits {
            semantic_rows: 16,
            ..Default::default()
        },
    };
    let (client, hello) = ClientHandshake::begin(
        fixture.pin,
        fixture.principal,
        Purpose::Ordinary,
        config,
        fixture.device_key.clone(),
    )
    .unwrap();
    let config = HandshakeConfig {
        capabilities: vec![2, 3],
        limits: spaceterm_protocol::limits::NegotiatedLimits {
            semantic_rows: 8,
            ..Default::default()
        },
        ..Default::default()
    };
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
    let (_, ready) = server.receive_device(&fixture.pairings, &proof).unwrap();
    let Established::Ordinary(client) = client.receive_ready(&ready).unwrap() else {
        panic!("ordinary")
    };
    assert_eq!(client.major(), 2);
    assert_eq!(client.capabilities(), &[3]);
    assert_eq!(client.limits().semantic_rows, 8);
}

fn signed_server_bytes(
    fixture: &Fixture,
    domain: &[u8],
    parts: &[&[u8]],
    body: Vec<u8>,
) -> Vec<u8> {
    use spaceterm_protocol::authentication::SigningKey;
    let mut transcript = Vec::new();
    for component in [b"SpaceTerm Protocol Ed25519 v1".as_slice(), domain]
        .into_iter()
        .chain(parts.iter().copied())
    {
        transcript.extend_from_slice(&(component.len() as u64).to_be_bytes());
        transcript.extend_from_slice(component);
    }
    common::Signed {
        body,
        signature: fixture.server_key.sign(&transcript).unwrap().to_vec(),
    }
    .encode_to_vec()
}

#[test]
fn an_authentic_server_signature_does_not_authorize_a_downgraded_selection() {
    let fixture = Fixture::new();
    let (client, hello) = ClientHandshake::begin(
        fixture.pin,
        fixture.principal,
        Purpose::Ordinary,
        HandshakeConfig::default(),
        fixture.device_key.clone(),
    )
    .unwrap();
    let (_, reply) = ServerHandshake::accept(
        fixture.principal.server,
        7,
        HandshakeConfig::default(),
        fixture.server_key.clone(),
        &fixture.pairings,
        &hello,
    )
    .unwrap();
    let signed = common::Signed::decode(reply.as_slice()).unwrap();
    let mut reply = common::ServerHello::decode(signed.body.as_slice()).unwrap();
    reply.selected_major = 1;
    let body = reply.encode_to_vec();
    let bytes = signed_server_bytes(&fixture, b"server-hello", &[&hello, &body], body.clone());
    assert!(matches!(
        client.receive_server(&bytes),
        Err(ProtocolError::Negotiation)
    ));
}

#[test]
fn signed_readiness_must_bind_the_original_client_device_proof_and_purpose() {
    let fixture = Fixture::new();
    for changed_purpose in [false, true] {
        let (client, hello) = ClientHandshake::begin(
            fixture.pin,
            fixture.principal,
            Purpose::Ordinary,
            HandshakeConfig::default(),
            fixture.device_key.clone(),
        )
        .unwrap();
        let (server, reply) = ServerHandshake::accept(
            fixture.principal.server,
            7,
            HandshakeConfig::default(),
            fixture.server_key.clone(),
            &fixture.pairings,
            &hello,
        )
        .unwrap();
        let reply_body = common::Signed::decode(reply.as_slice()).unwrap().body;
        let (client, proof) = client.receive_server(&reply).unwrap();
        let (_, ready) = server.receive_device(&fixture.pairings, &proof).unwrap();
        let signed = common::Signed::decode(ready.as_slice()).unwrap();
        let mut ready = common::Ready::decode(signed.body.as_slice()).unwrap();
        if changed_purpose {
            ready.purpose = common::Purpose::RevocationStatus as i32;
        } else {
            ready.device_proof[0] ^= 1;
        }
        let body = ready.encode_to_vec();
        let bytes = signed_server_bytes(
            &fixture,
            b"ready",
            &[&hello, &reply_body, &body],
            body.clone(),
        );
        assert!(matches!(
            client.receive_ready(&bytes),
            Err(ProtocolError::Authentication)
        ));
    }
}

#[test]
fn revocation_during_native_ready_signing_does_not_publish_ordinary_readiness() {
    use spaceterm_protocol::authentication::{PairingLookup, SigningKey};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    struct LivePairing {
        record: PairingRecord,
        active: Arc<AtomicBool>,
    }
    impl PairingLookup for LivePairing {
        fn lookup(
            &self,
            _: [u8; 16],
            _: [u8; 16],
            _: [u8; 16],
        ) -> Result<PairingRecord, ProtocolError> {
            Ok(PairingRecord {
                active: self.active.load(Ordering::SeqCst),
                ..self.record
            })
        }
    }
    struct NativeSigner {
        key: Arc<TestKey>,
        active: Arc<AtomicBool>,
        calls: AtomicUsize,
    }
    impl SigningKey for NativeSigner {
        fn public_key(&self) -> [u8; 32] {
            self.key.public_key()
        }
        fn sign(&self, transcript: &[u8]) -> Result<[u8; 64], ProtocolError> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 1 {
                self.active.store(false, Ordering::SeqCst);
            }
            self.key.sign(transcript)
        }
    }
    let fixture = Fixture::new();
    let active = Arc::new(AtomicBool::new(true));
    let pairings = LivePairing {
        record: fixture.pairings.record,
        active: active.clone(),
    };
    let signer = Arc::new(NativeSigner {
        key: fixture.server_key.clone(),
        active,
        calls: AtomicUsize::new(0),
    });
    let (client, hello) = ClientHandshake::begin(
        fixture.pin,
        fixture.principal,
        Purpose::Ordinary,
        HandshakeConfig::default(),
        fixture.device_key.clone(),
    )
    .unwrap();
    let (server, reply) = ServerHandshake::accept(
        fixture.principal.server,
        7,
        HandshakeConfig::default(),
        signer,
        &pairings,
        &hello,
    )
    .unwrap();
    let (_, proof) = client.receive_server(&reply).unwrap();
    assert!(matches!(
        server.receive_device(&pairings, &proof),
        Err(ProtocolError::PairingDenied)
    ));
}
