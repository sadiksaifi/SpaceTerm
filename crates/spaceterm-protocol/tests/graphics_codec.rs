use spaceterm_core::{authority::*, content::*, content_queries::*, identity::*};
use spaceterm_protocol::convert::content;

fn request() -> GraphicsRequest {
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
    GraphicsRequest::new(
        QueryStamp::new(
            fence,
            base,
            ContentRequestId::new(12).unwrap(),
            RequestGeneration::new(13),
        )
        .unwrap(),
        ResourceTransferId::new(14).unwrap(),
        GraphicsResourceRef {
            key: GraphicsResourceKey {
                session,
                id: GraphicsResourceId::new(15).unwrap(),
                generation: 16,
            },
            width: 2,
            height: 1,
            rgba_bytes: 8,
            digest: [17; 32],
        },
        0,
    )
    .unwrap()
}

#[test]
fn both_majors_preserve_exact_resource_request_and_chunk_identity() {
    macro_rules! check {
        ($p:ident) => {{
            let request = request();
            let encoded = content::$p::encode_graphics_request(request);
            assert_eq!(
                content::$p::decode_graphics_request(&encoded).unwrap(),
                request
            );
            for chunk in [
                GraphicsChunk::new(request, 0, vec![1; 4], false).unwrap(),
                GraphicsChunk::new(request, 4, vec![2; 4], true).unwrap(),
            ] {
                let encoded = content::$p::encode_graphics_chunk(&chunk);
                assert_eq!(
                    content::$p::decode_graphics_chunk(&encoded, request).unwrap(),
                    chunk
                );
            }
        }};
    }
    check!(v1);
    check!(v2);
}

#[test]
fn both_majors_reject_retagged_requests_and_resource_chunks() {
    macro_rules! check {
        ($p:ident) => {{
            let request = request();
            let original = content::$p::encode_graphics_request(request);
            for scenario in 0..5 {
                let mut value = original.clone();
                match scenario {
                    0 => value.fence.as_mut().unwrap().context_generation += 1,
                    1 => value.resource.as_mut().unwrap().local_id = 0,
                    2 => value.rgba_bytes += 1,
                    3 => value.offset = u64::MAX,
                    _ => value.digest.pop().map(|_| ()).unwrap(),
                }
                assert!(content::$p::decode_graphics_request(&value).is_err());
            }
            let original = content::$p::encode_graphics_chunk(
                &GraphicsChunk::new(request, 0, vec![1; 8], true).unwrap(),
            );
            for scenario in 0..8 {
                let mut value = original.clone();
                match scenario {
                    0 => value.transfer += 1,
                    1 => value.fence.as_mut().unwrap().context_generation += 1,
                    2 => value.resource.as_mut().unwrap().generation += 1,
                    3 => value.digest[0] ^= 1,
                    4 => value.width += 1,
                    5 => value.format = 2,
                    6 => value.offset = u64::MAX,
                    _ => value.complete = false,
                }
                assert!(content::$p::decode_graphics_chunk(&value, request).is_err());
            }
        }};
    }
    check!(v1);
    check!(v2);
}
