use spaceterm_core::{authority::*, content::*, content_queries::*, identity::*};
use spaceterm_protocol::{
    codec::Payload,
    convert::content::reads,
    wire::{v1, v2},
};
use spaceterm_server::egress::{ConnectionEgress, EgressBudget, EgressFailure};

fn request() -> CopyRequest {
    let server = ServerId::new([1; 16]);
    let pane = PaneId::new(server, 2).unwrap();
    let client = ClientId::new(server, 3).unwrap();
    let session = SessionKey {
        pane,
        epoch: ServerEpoch::new(4),
        generation: SessionGeneration::new(5),
    };
    let base = ContentBase {
        session,
        revision: ContentRevision::new(6),
        screen: Screen::Primary,
        screen_generation: 7,
        topology_revision: 8,
    };
    let fence = EffectFence::new(
        session,
        ControlGrant::new(pane, client, ControlGeneration::new(9)).unwrap(),
        ConnectionBinding::new(client, ConnectionGeneration::new(10)),
        ContextGeneration::new(11),
        Some(base.revision),
    )
    .unwrap();
    let stamp = QueryStamp::new(
        fence,
        base,
        ContentRequestId::new(12).unwrap(),
        RequestGeneration::new(13),
    )
    .unwrap();
    let cell = CellRef {
        session,
        row: RowId {
            screen_generation: 7,
            screen: Screen::Primary,
            node_serial: 14,
            page_row: 1,
        },
        row_revision: 15,
        column: 0,
    };
    CopyRequest::new(
        stamp,
        ContentSelection::new(stamp, cell, cell, false).unwrap(),
        CopyOptions {
            unwrap_soft_wraps: true,
            trim_trailing_spaces: true,
            include_html: false,
        },
    )
    .unwrap()
}
#[test]
fn both_majors_charge_copy_and_find_streams_as_normal_egress() {
    macro_rules! check {
        ($p:ident, $major:ident) => {{
            let request = request();
            let copy = CopyStreamItem::Chunk(
                CopyChunk::new(
                    &request,
                    ResourceTransferId::new(16).unwrap(),
                    CopyPart::Plain,
                    0,
                    3,
                    b"abc".to_vec(),
                )
                .unwrap(),
            );
            let find = FindStreamItem::Complete {
                stamp: request.stamp(),
                chunks: 0,
            };
            let mut egress = ConnectionEgress::new(EgressBudget::new());
            for payload in [
                Payload::$major($p::Message {
                    kind: Some($p::message::Kind::CopyEvent(reads::$p::encode_copy_event(
                        &copy,
                    ))),
                }),
                Payload::$major($p::Message {
                    kind: Some($p::message::Kind::FindEvent(reads::$p::encode_find_event(
                        &find,
                    ))),
                }),
            ] {
                let normal = egress.reserve_content_reply(8192).unwrap();
                egress
                    .validate_reply_reservation(&payload, &normal)
                    .unwrap();
                let reliable = egress.reserve_reliable(8192).unwrap();
                assert_eq!(
                    egress.validate_reply_reservation(&payload, &reliable),
                    Err(EgressFailure::ReservationMismatch)
                );
            }
        }};
    }
    check!(v1, Major1);
    check!(v2, Major2);
}
#[test]
fn saturated_normal_egress_can_still_send_content_free_failure_metadata() {
    macro_rules! check {
        ($p:ident, $major:ident) => {{
            let request = request();
            let global = EgressBudget::new();
            let mut egress = ConnectionEgress::new(global);
            let mut held = Vec::new();
            while let Ok(reservation) = egress.reserve_content_reply(64 * 1024) {
                held.push(reservation);
            }
            // Exhaust normal bytes at the minimum reservation size too.
            while let Ok(reservation) = egress.reserve_content_reply(4096) {
                held.push(reservation);
            }
            let failure = CopyStreamItem::Failed {
                stamp: request.stamp(),
                reason: ContentReadFailure::Capacity,
            };
            let payload = Payload::$major($p::Message {
                kind: Some($p::message::Kind::CopyEvent(reads::$p::encode_copy_event(
                    &failure,
                ))),
            });
            let reserved = egress.reserve_reliable(4096).unwrap();
            egress
                .validate_reply_reservation(&payload, &reserved)
                .unwrap();
            assert!(egress.reserve_content_reply(4096).is_err());
        }};
    }
    check!(v1, Major1);
    check!(v2, Major2);
}
