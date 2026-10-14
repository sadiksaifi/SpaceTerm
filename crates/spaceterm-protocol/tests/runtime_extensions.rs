#[expect(
    dead_code,
    reason = "shared authentication fixture serves other suites"
)]
mod support;

use spaceterm_core::{authority::*, content::*, content_queries::*, observations::*};
use spaceterm_protocol::{
    ProtocolError,
    authentication::{ClientHandshake, Established, HandshakeConfig, Purpose, ServerHandshake},
    codec::Payload,
    convert::{metadata, runtime},
    negotiation::{CAPABILITY_APPLICATION_MOUSE, CAPABILITY_RUNTIME_METADATA},
    wire::{v1, v2},
};
use spaceterm_terminal::{
    InputModifiers,
    metadata::{MetadataFreshness, ProgressMetadata, PromptZone, TitleMetadata, TitleProvenance},
    pointer_input::{PointerButton, PointerPhase},
    runtime_contracts::*,
};
use support::Fixture;

fn fence(content: Option<ContentRevision>) -> EffectFence {
    use spaceterm_core::identity::{ClientId, PaneId, ServerId};
    let server = ServerId::new([1; 16]);
    let pane = PaneId::new(server, 1).unwrap();
    let client = ClientId::new(server, 4).unwrap();
    EffectFence::new(
        SessionKey {
            pane,
            epoch: ServerEpoch::new(7),
            generation: SessionGeneration::new(3),
        },
        ControlGrant::new(pane, client, ControlGeneration::new(2)).unwrap(),
        ConnectionBinding::new(client, ConnectionGeneration::new(3)),
        ContextGeneration::new(4),
        content,
    )
    .unwrap()
}
fn mouse() -> FencedRuntimeCommand {
    let fence = fence(Some(ContentRevision::new(0)));
    let base = ContentBase {
        session: fence.session(),
        revision: ContentRevision::new(0),
        screen: Screen::Primary,
        screen_generation: 0,
        topology_revision: 0,
    };
    let stamp = QueryStamp::new(
        fence,
        base,
        ContentRequestId::new(1).unwrap(),
        RequestGeneration::new(0),
    )
    .unwrap();
    let cell = CellRef {
        session: base.session,
        row: RowId {
            screen: base.screen,
            screen_generation: 0,
            node_serial: 0,
            page_row: 0,
        },
        row_revision: 0,
        column: 0,
    };
    FencedRuntimeCommand {
        fence,
        command: RuntimeCommand::Pointer(ApplicationPointerInput {
            stamp,
            cell,
            phase: PointerPhase::Press,
            button: Some(PointerButton::Left),
            modifiers: InputModifiers::default(),
        }),
    }
}
fn snapshot(sequence: u64, progress: ProgressMetadata) -> RuntimeMetadataSnapshot {
    RuntimeMetadataSnapshot::new(RuntimeMetadataParts {
        session: mouse().fence.session(),
        sequence: RuntimeObservationSequence::new(sequence).unwrap(),
        observed_at: MonotonicTick::new(sequence),
        title: TitleMetadata {
            value: "runtime title".into(),
            provenance: TitleProvenance::TerminalControl,
        },
        origin: RuntimeMetadataOrigin::Remote {
            user: Some(RuntimeMetadataIdentity::new("user".into()).unwrap()),
            host: RuntimeMetadataIdentity::new("host".into()).unwrap(),
        },
        progress,
        freshness: MetadataFreshness::Stale,
        prompt_zone: PromptZone::CommandOutput,
        hidden_input: true,
    })
    .unwrap()
}
fn connected(
    major: u32,
    capabilities: Vec<u32>,
) -> (
    spaceterm_protocol::connection::AuthenticatedConnection,
    spaceterm_protocol::connection::AuthenticatedConnection,
) {
    let fixture = Fixture::new();
    let config = HandshakeConfig {
        majors: vec![major],
        capabilities,
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
    let (Established::Ordinary(client), Established::Ordinary(server)) =
        (client.receive_ready(&ready).unwrap(), server)
    else {
        panic!("ordinary connections")
    };
    (client, server)
}

macro_rules! extensions {
    ($name:ident, $p:ident, $major:ident, $number:literal) => {
        #[test]
        fn $name() {
            let command = mouse();
            let mouse_payload = Payload::$major($p::Message {
                kind: Some($p::message::Kind::RuntimeCommand(
                    runtime::$p::encode_command(1, &command).unwrap(),
                )),
            });
            let retained_fence = fence(None);
            for progress in [
                ProgressMetadata::None,
                ProgressMetadata::Normal(0),
                ProgressMetadata::Normal(100),
                ProgressMetadata::Error(42),
                ProgressMetadata::Paused(43),
                ProgressMetadata::Indeterminate,
            ] {
                let expected = snapshot(2, progress);
                let encoded =
                    metadata::$p::encode_publication(retained_fence, 3, 4, &expected).unwrap();
                assert_eq!(
                    metadata::$p::decode_publication(&encoded).unwrap(),
                    (retained_fence, expected.clone())
                );
                let publication = Payload::$major($p::Message {
                    kind: Some($p::message::Kind::MetadataPublication(Box::new(encoded))),
                });
                let (mut client, mut server) = connected($number, vec![]);
                assert_eq!(
                    client.encode(&mouse_payload),
                    Err(ProtocolError::Negotiation)
                );
                assert_eq!(server.encode(&publication), Err(ProtocolError::Negotiation));
                let (mut client, mut server) = connected(
                    $number,
                    vec![CAPABILITY_APPLICATION_MOUSE, CAPABILITY_RUNTIME_METADATA],
                );
                assert_eq!(
                    server
                        .decode(&client.encode(&mouse_payload).unwrap())
                        .unwrap(),
                    mouse_payload
                );
                assert_eq!(
                    client
                        .decode(&server.encode(&publication).unwrap())
                        .unwrap(),
                    publication
                );
                assert_eq!(
                    client.encode(&publication),
                    Err(ProtocolError::UnexpectedMessage)
                );
                let mut invalid = metadata::encode_snapshot(&expected);
                invalid.sequence = 0;
                assert_eq!(
                    metadata::decode_snapshot(&invalid),
                    Err(ProtocolError::Malformed)
                );
                invalid = metadata::encode_snapshot(&expected);
                invalid.progress_kind = 1;
                invalid.progress_percent = 101;
                assert_eq!(
                    metadata::decode_snapshot(&invalid),
                    Err(ProtocolError::Malformed)
                );
                invalid = metadata::encode_snapshot(&expected);
                invalid.title = "\u{1b}title".into();
                assert_eq!(
                    metadata::decode_snapshot(&invalid),
                    Err(ProtocolError::Malformed)
                );
            }
            let mut wire = runtime::$p::encode_command(2, &command).unwrap();
            let Some($p::runtime_command::Action::ApplicationPointer(input)) = &mut wire.action
            else {
                panic!("pointer action")
            };
            input
                .cell
                .as_mut()
                .unwrap()
                .physical_row
                .as_mut()
                .unwrap()
                .screen = 2;
            assert_eq!(
                runtime::$p::decode_command(&wire),
                Err(ProtocolError::Malformed)
            );
            let mut wire = runtime::$p::encode_command(2, &command).unwrap();
            let Some($p::runtime_command::Action::ApplicationPointer(input)) = &mut wire.action
            else {
                panic!("pointer action")
            };
            input
                .stamp
                .as_mut()
                .unwrap()
                .fence
                .as_mut()
                .unwrap()
                .context_generation += 1;
            assert_eq!(
                runtime::$p::decode_command(&wire),
                Err(ProtocolError::Malformed)
            );
        }
    };
}
extensions!(
    mouse_and_metadata_are_checked_capability_gated_major_1,
    v1,
    Major1,
    1
);
extensions!(
    mouse_and_metadata_are_checked_capability_gated_major_2,
    v2,
    Major2,
    2
);
