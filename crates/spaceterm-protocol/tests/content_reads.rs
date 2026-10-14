use spaceterm_core::{authority::*, content::*, content_queries::*, identity::*};
use spaceterm_protocol::convert::content::{self, reads::ReadQuery};

fn stamp() -> QueryStamp {
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
    QueryStamp::new(
        fence,
        base,
        ContentRequestId::new(12).unwrap(),
        RequestGeneration::new(13),
    )
    .unwrap()
}
fn row() -> TopologyRow {
    TopologyRow::new(
        RowId {
            screen_generation: 7,
            screen: Screen::Primary,
            node_serial: 14,
            page_row: 1,
        },
        15,
        100,
        true,
    )
    .unwrap()
}
fn cell() -> CellRef {
    CellRef {
        session: stamp().base().session,
        row: row().row(),
        row_revision: row().revision(),
        column: 16,
    }
}
fn find() -> FindRequest {
    let query = BoundedFindQuery::new("literal é😀".into()).unwrap();
    FindRequest::new(
        stamp(),
        query.clone(),
        Some(FindContinuation::new(stamp(), &query, cell()).unwrap()),
    )
    .unwrap()
}
fn copy() -> CopyRequest {
    CopyRequest::new(
        stamp(),
        ContentSelection::new(stamp(), cell(), cell(), true).unwrap(),
        CopyOptions {
            unwrap_soft_wraps: true,
            trim_trailing_spaces: false,
            include_html: true,
        },
    )
    .unwrap()
}
#[test]
fn both_majors_preserve_copy_find_topology_and_cancellation_bindings() {
    let queries = [
        ReadQuery::Copy(copy()),
        ReadQuery::Find(find()),
        ReadQuery::Topology(
            TopologyPageRequest::new(
                stamp(),
                Some(TopologyContinuation::new(stamp(), row()).unwrap()),
                PageLimits::new(256, 16384, 128 * 1024).unwrap(),
            )
            .unwrap(),
        ),
        ReadQuery::Cancel {
            stamp: stamp(),
            kind: 5,
        },
    ];
    macro_rules! check {
        ($p:ident) => {
            for query in &queries {
                let encoded = content::reads::$p::encode_query(query);
                assert_eq!(content::reads::$p::decode_query(&encoded).unwrap(), *query);
                assert!(content::$p::decode_page_request(&encoded).is_err());
            }
        };
    }
    check!(v1);
    check!(v2);
}
#[test]
fn both_majors_reject_inconsistent_find_continuations_and_unsupported_modes() {
    macro_rules! check {
        ($p:ident) => {{
            let encoded = content::reads::$p::encode_query(&ReadQuery::Find(find()));
            for scenario in 0..8 {
                let mut value = encoded.clone();
                match scenario {
                    0 => value.query.push('x'),
                    1 => value.find_continuation.as_mut().unwrap().query_fingerprint[0] ^= 1,
                    2 => {
                        value
                            .find_continuation
                            .as_mut()
                            .unwrap()
                            .stamp
                            .as_mut()
                            .unwrap()
                            .request_generation += 1
                    }
                    3 => value.fence.as_mut().unwrap().context_generation += 1,
                    4 => value.regex = true,
                    5 => value.case_sensitive = true,
                    6 => value.query = "x".repeat(MAX_FIND_QUERY_BYTES + 1),
                    _ => value.cancel = true,
                }
                assert!(content::reads::$p::decode_query(&value).is_err());
            }
        }};
    }
    check!(v1);
    check!(v2);
}
#[test]
fn both_majors_reject_copy_endpoints_outside_the_original_content_base() {
    macro_rules! check {
        ($p:ident) => {{
            let encoded = content::reads::$p::encode_query(&ReadQuery::Copy(copy()));
            for scenario in 0..5 {
                let mut value = encoded.clone();
                match scenario {
                    0 => {
                        value
                            .end
                            .as_mut()
                            .unwrap()
                            .session
                            .as_mut()
                            .unwrap()
                            .generation += 1
                    }
                    1 => {
                        value
                            .end
                            .as_mut()
                            .unwrap()
                            .physical_row
                            .as_mut()
                            .unwrap()
                            .screen_generation += 1
                    }
                    2 => value.end.as_mut().unwrap().column = u32::MAX,
                    3 => value.start = None,
                    _ => value.copy_options = None,
                }
                assert!(content::reads::$p::decode_query(&value).is_err());
            }
        }};
    }
    check!(v1);
    check!(v2);
}

#[test]
fn both_majors_decode_bounded_read_streams_and_reject_rebound_replies() {
    let topology = TopologyPageRequest::new(
        stamp(),
        None,
        PageLimits::new(256, 16384, 128 * 1024).unwrap(),
    )
    .unwrap();
    let page = ContentReadResult::Ready(
        TopologyPage::new(
            &topology,
            vec![row()],
            Some(TopologyContinuation::new(stamp(), row()).unwrap()),
        )
        .unwrap(),
    );
    let copies = [
        CopyStreamItem::Chunk(
            CopyChunk::new(
                &copy(),
                ResourceTransferId::new(1).unwrap(),
                CopyPart::Plain,
                0,
                2,
                b"ok".to_vec(),
            )
            .unwrap(),
        ),
        CopyStreamItem::Complete(
            CopyCompletion::new(
                &copy(),
                ResourceTransferId::new(1).unwrap(),
                2,
                [2; 32],
                Some((0, [3; 32])),
            )
            .unwrap(),
        ),
        CopyStreamItem::Failed {
            stamp: stamp(),
            reason: ContentReadFailure::Cancelled,
        },
    ];
    let finds = [
        FindStreamItem::Chunk(Box::new(
            FindChunk::new(
                &find(),
                0,
                vec![FindMatch::new(stamp(), cell(), cell()).unwrap()],
                find().continuation(),
            )
            .unwrap(),
        )),
        FindStreamItem::Complete {
            stamp: stamp(),
            chunks: 1,
        },
        FindStreamItem::Failed {
            stamp: stamp(),
            reason: ContentReadFailure::Retired,
        },
    ];
    macro_rules! check {
        ($p:ident) => {{
            let encoded = content::reads::$p::encode_topology_result(&page);
            assert_eq!(
                content::reads::$p::decode_topology_result(&encoded, &topology).unwrap(),
                page
            );
            for item in &copies {
                let encoded = content::reads::$p::encode_copy_event(item);
                assert_eq!(
                    content::reads::$p::decode_copy_event(&encoded, &copy()).unwrap(),
                    *item
                );
            }
            for item in &finds {
                let encoded = content::reads::$p::encode_find_event(item);
                assert_eq!(
                    content::reads::$p::decode_find_event(&encoded, &find()).unwrap(),
                    *item
                );
            }
            let mut encoded = content::reads::$p::encode_copy_event(&copies[0]);
            let Some(spaceterm_protocol::wire::$p::copy_event::Item::Chunk(chunk)) =
                encoded.item.as_mut()
            else {
                panic!("Copy chunk")
            };
            chunk.stamp.as_mut().unwrap().request_generation += 1;
            assert!(content::reads::$p::decode_copy_event(&encoded, &copy()).is_err());
            for scenario in 0..5 {
                let mut encoded = content::reads::$p::encode_copy_event(&copies[1]);
                let Some(spaceterm_protocol::wire::$p::copy_event::Item::Complete(value)) =
                    encoded.item.as_mut()
                else {
                    panic!("Copy completion")
                };
                match scenario {
                    0 => value.html_digest = None,
                    1 => {
                        value.plain_digest.pop();
                    }
                    2 => value.transfer = 0,
                    3 => value.plain_bytes = u64::MAX,
                    _ => value.stamp.as_mut().unwrap().request_generation += 1,
                }
                assert!(content::reads::$p::decode_copy_event(&encoded, &copy()).is_err());
            }
            let mut encoded = content::reads::$p::encode_find_event(&finds[0]);
            let Some(spaceterm_protocol::wire::$p::find_event::Item::Chunk(chunk)) =
                encoded.item.as_mut()
            else {
                panic!("Find chunk")
            };
            chunk.continuation.as_mut().unwrap().query_fingerprint[0] ^= 1;
            assert!(content::reads::$p::decode_find_event(&encoded, &find()).is_err());
            let mut encoded = content::reads::$p::encode_topology_result(&page);
            let Some(spaceterm_protocol::wire::$p::content_read_response::Result::Topology(page)) =
                encoded.result.as_mut()
            else {
                panic!("Topology page")
            };
            page.rows.push(page.rows[0].clone());
            assert!(content::reads::$p::decode_topology_result(&encoded, &topology).is_err());
        }};
    }
    check!(v1);
    check!(v2);
}
