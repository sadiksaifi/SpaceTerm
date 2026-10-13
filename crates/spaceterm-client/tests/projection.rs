#![cfg(feature = "test-support")]
#[path = "projection/support.rs"]
mod support;
use sha2::{Digest, Sha256};
use spaceterm_client::{
    failure::ClientFailureClass,
    pane_channel::*,
    ports::ConnectionInvalidation,
    projection::{budget::Residency, *},
};
use spaceterm_core::{authority::ContentRevision, content::*, content_queries::*};
use support::*;

#[tokio::test]
async fn complete_rich_copy_above_semantic_cache_and_paste_limits_has_separate_leases() {
    let semantic = Residency::new(8192, 8192).unwrap();
    let copies = Residency::new(8 * 1024 * 1024, 8 * 1024 * 1024).unwrap();
    let fixture = Fixture::with_budgets(
        true,
        ProjectionBudgets {
            semantic: semantic.clone(),
            prepared_copy: copies.clone(),
            graphics: Residency::new(16, 16).unwrap(),
        },
    )
    .await;
    let stamp = fixture.frame(1);
    let request = copy_request(stamp, true);
    let projection = fixture.pane.projection();
    projection
        .lock()
        .unwrap()
        .set_selection(Some(request.selection()))
        .unwrap();
    let mut preparation = projection.lock().unwrap().begin_copy(request).unwrap();
    let transfer = ResourceTransferId::new(1).unwrap();
    let plain = "x".repeat(2 * 1024 * 1024 + 17);
    let html = "y".repeat(3 * 1024 * 1024 + 29);
    for (part, text) in [(CopyPart::Plain, &plain), (CopyPart::Html, &html)] {
        for (index, bytes) in text.as_bytes().chunks(MAX_RESOURCE_CHUNK_BYTES).enumerate() {
            assert!(
                preparation
                    .accept(CopyStreamItem::Chunk(
                        CopyChunk::new(
                            &request,
                            transfer,
                            part,
                            (index * MAX_RESOURCE_CHUNK_BYTES) as u64,
                            text.len() as u64,
                            bytes.to_vec()
                        )
                        .unwrap()
                    ))
                    .unwrap()
                    .is_none()
            );
            assert!(projection.lock().unwrap().prepared_copy().is_none());
        }
    }
    let completion = CopyCompletion::new(
        &request,
        transfer,
        plain.len() as u64,
        Sha256::digest(plain.as_bytes()).into(),
        Some((html.len() as u64, Sha256::digest(html.as_bytes()).into())),
    )
    .unwrap();
    let lease = preparation
        .accept(CopyStreamItem::Complete(completion))
        .unwrap()
        .unwrap();
    projection
        .lock()
        .unwrap()
        .install_copy(lease.clone())
        .unwrap();
    assert_eq!(lease.plain(), Some(plain.as_str()));
    assert_eq!(lease.html(), Some(html.as_str()));
    assert!(semantic.held_bytes().unwrap() < 8192);
    assert!(copies.held_bytes().unwrap() > 5 * 1024 * 1024);
    projection.lock().unwrap().set_selection(None).unwrap();
    assert!(lease.plain().is_none());
    assert!(copies.held_bytes().unwrap() > 0);
    drop(lease);
    assert_eq!(copies.held_bytes().unwrap(), 0);
}
#[tokio::test]
async fn incomplete_out_of_order_digest_failed_and_allocation_rejected_copy_never_becomes_eligible()
{
    for scenario in 0..4 {
        let copies = Residency::new(8, 8).unwrap();
        let fixture = Fixture::with_budgets(
            true,
            ProjectionBudgets {
                semantic: Residency::new(8192, 8192).unwrap(),
                prepared_copy: copies.clone(),
                graphics: Residency::new(16, 16).unwrap(),
            },
        )
        .await;
        let stamp = fixture.frame(1);
        let request = copy_request(stamp, false);
        let projection = fixture.pane.projection();
        projection
            .lock()
            .unwrap()
            .set_selection(Some(request.selection()))
            .unwrap();
        let mut preparation = projection.lock().unwrap().begin_copy(request).unwrap();
        let transfer = ResourceTransferId::new(1).unwrap();
        let chunk = CopyChunk::new(
            &request,
            transfer,
            CopyPart::Plain,
            if scenario == 1 { 1 } else { 0 },
            if scenario == 3 { 9 } else { 8 },
            if scenario == 3 {
                vec![b'x'; 9]
            } else {
                vec![b'x'; 4]
            },
        )
        .unwrap();
        let result = preparation.accept(CopyStreamItem::Chunk(chunk));
        if scenario == 1 || scenario == 3 {
            assert!(result.is_err());
        } else {
            result.unwrap();
            let completion = CopyCompletion::new(
                &request,
                transfer,
                if scenario == 0 { 8 } else { 4 },
                [0; 32],
                None,
            )
            .unwrap();
            assert!(
                preparation
                    .accept(CopyStreamItem::Complete(completion))
                    .is_err()
            );
        }
        assert!(projection.lock().unwrap().prepared_copy().is_none());
        assert_eq!(copies.held_bytes().unwrap(), 0);
    }
}
#[tokio::test]
async fn takeover_removes_frame_copy_services_accessibility_and_ime_eligibility() {
    let fixture = Fixture::new(true).await;
    let stamp = fixture.frame(1);
    let frame = fixture.pane.current_frame().unwrap();
    let request = copy_request(stamp, false);
    let projection = fixture.pane.projection();
    projection
        .lock()
        .unwrap()
        .set_selection(Some(request.selection()))
        .unwrap();
    let mut preparation = projection.lock().unwrap().begin_copy(request).unwrap();
    let lease = preparation
        .accept(CopyStreamItem::Complete(
            CopyCompletion::new(
                &request,
                ResourceTransferId::new(1).unwrap(),
                0,
                Sha256::digest([]).into(),
                None,
            )
            .unwrap(),
        ))
        .unwrap()
        .unwrap();
    projection
        .lock()
        .unwrap()
        .install_copy(lease.clone())
        .unwrap();
    fixture.takeover().await;
    assert!(frame.frame().is_none());
    assert!(frame.accessibility().is_none());
    assert!(lease.plain().is_none());
    assert!(fixture.pane.permit(EffectKind::ImeCommit).is_err());
}
#[tokio::test]
async fn mixed_content_revisions_require_resync_without_installing_partial_frame() {
    let fixture = Fixture::new(true).await;
    let stamp = fixture.frame(1);
    let projection = fixture.pane.projection();
    projection
        .lock()
        .unwrap()
        .accept_page(stamp, page(stamp.base(), 1, true))
        .unwrap();
    let other = ContentBase {
        topology_revision: 2,
        ..stamp.base()
    };
    let other_stamp =
        QueryStamp::new(stamp.fence(), other, stamp.request(), stamp.generation()).unwrap();
    assert!(
        projection
            .lock()
            .unwrap()
            .accept_page(other_stamp, page(other, 2, false))
            .is_err()
    );
    assert!(projection.lock().unwrap().needs_content_resync());
    assert_eq!(
        fixture
            .pane
            .current_frame()
            .unwrap()
            .frame()
            .unwrap()
            .base(),
        stamp.base()
    );
}
#[tokio::test]
async fn retained_snapshot_leases_are_counted_in_semantic_residency() {
    let bytes = 4364;
    let semantic = Residency::new(bytes * 2, bytes * 2).unwrap();
    let fixture = Fixture::with_budgets(
        true,
        ProjectionBudgets {
            semantic: semantic.clone(),
            prepared_copy: Residency::new(16, 16).unwrap(),
            graphics: Residency::new(16, 16).unwrap(),
        },
    )
    .await;
    fixture.frame(1);
    let old = fixture.pane.current_frame().unwrap();
    fixture.frame(2);
    let newer = fixture.pane.current_frame().unwrap();
    let fence = fixture.pane.permit(EffectKind::Key).unwrap().fence();
    let base = ContentBase {
        revision: ContentRevision::new(3),
        ..newer.frame().unwrap().base()
    };
    fixture.pane.publish_content_base(fence, base).unwrap();
    let permit = fixture.pane.permit(EffectKind::Read).unwrap();
    let stamp = fixture
        .pane
        .query_stamp(&permit, RequestGeneration::new(1))
        .unwrap();
    assert_eq!(
        fixture
            .pane
            .projection()
            .lock()
            .unwrap()
            .accept_page(stamp, page(base, 1, false))
            .unwrap_err()
            .class(),
        ClientFailureClass::ResourceLimit
    );
    drop(old);
    drop(newer);
    fixture
        .pane
        .projection()
        .lock()
        .unwrap()
        .accept_page(stamp, page(base, 1, false))
        .unwrap();
}
#[tokio::test]
async fn personal_selection_scroll_and_find_do_not_mutate_shared_content_or_journal() {
    let fixture = Fixture::new(true).await;
    let stamp = fixture.frame(1);
    let before = fixture.server.snapshot().unwrap();
    let journal = fixture.peer.journal().snapshot().unwrap();
    let projection = fixture.pane.projection();
    let mut projection = projection.lock().unwrap();
    projection.set_selection(Some(selection(stamp))).unwrap();
    projection
        .set_viewport(Viewport {
            anchor: Some(row(stamp.base(), 1)),
            rows: 30,
        })
        .unwrap();
    let generation = projection.find().generation();
    let find_stamp =
        QueryStamp::new(stamp.fence(), stamp.base(), stamp.request(), generation).unwrap();
    let request = FindRequest::new(
        find_stamp,
        BoundedFindQuery::new("literal".into()).unwrap(),
        None,
    )
    .unwrap();
    projection
        .find_mut()
        .unwrap()
        .begin(request.clone())
        .unwrap();
    for index in 0..3 {
        let matched =
            FindMatch::new(find_stamp, selection(stamp).start(), selection(stamp).end()).unwrap();
        projection
            .find_mut()
            .unwrap()
            .accept(FindStreamItem::Chunk(Box::new(
                FindChunk::new(&request, index, vec![matched], None).unwrap(),
            )))
            .unwrap();
    }
    projection
        .find_mut()
        .unwrap()
        .accept(FindStreamItem::Complete {
            stamp: find_stamp,
            chunks: 3,
        })
        .unwrap();
    assert!(matches!(
        projection.find_mut().unwrap().navigate(true),
        FindNavigation::Match(_)
    ));
    assert_eq!(
        projection.find_mut().unwrap().navigate(true),
        FindNavigation::LoadChunk { index: 1 }
    );
    drop(projection);
    assert_eq!(fixture.server.snapshot().unwrap(), before);
    assert_eq!(
        fixture.peer.journal().snapshot().unwrap().revision(),
        journal.revision()
    );
}
#[tokio::test]
async fn offline_prepared_copy_and_live_accessibility_actions_are_revoked() {
    let fixture = Fixture::new(true).await;
    let stamp = fixture.frame(1);
    let frame = fixture.pane.current_frame().unwrap();
    let projection = fixture.pane.projection();
    let request = copy_request(stamp, false);
    projection
        .lock()
        .unwrap()
        .set_selection(Some(request.selection()))
        .unwrap();
    let mut preparation = projection.lock().unwrap().begin_copy(request).unwrap();
    fixture
        .channels
        .invalidate(
            fixture.server.server(),
            ConnectionInvalidation::ConnectionLost,
        )
        .unwrap();
    assert!(
        preparation
            .accept(CopyStreamItem::Complete(
                CopyCompletion::new(
                    &request,
                    ResourceTransferId::new(1).unwrap(),
                    0,
                    Sha256::digest([]).into(),
                    None
                )
                .unwrap()
            ))
            .is_err()
    );
    let facts = frame.accessibility().unwrap();
    assert!(facts.frozen);
    assert!(!facts.live_actions);
}

#[tokio::test]
async fn dropping_presentation_revokes_external_frame_and_copy_leases_without_releasing_control() {
    let fixture = Fixture::new(true).await;
    let stamp = fixture.frame(1);
    let frame = fixture.pane.current_frame().unwrap();
    let request = copy_request(stamp, false);
    let projection = fixture.pane.projection();
    projection
        .lock()
        .unwrap()
        .set_selection(Some(request.selection()))
        .unwrap();
    let mut preparation = projection.lock().unwrap().begin_copy(request).unwrap();
    let copy = preparation
        .accept(CopyStreamItem::Complete(
            CopyCompletion::new(
                &request,
                ResourceTransferId::new(1).unwrap(),
                0,
                Sha256::digest([]).into(),
                None,
            )
            .unwrap(),
        ))
        .unwrap()
        .unwrap();
    let before = fixture.server.received_operations().unwrap().len();
    drop(fixture.pane);
    assert!(!frame.is_paintable());
    assert!(copy.plain().is_none());
    assert_eq!(fixture.server.received_operations().unwrap().len(), before);
}

#[tokio::test]
async fn unfocused_pane_keeps_paint_but_has_no_native_content_eligibility() {
    let fixture = Fixture::new(true).await;
    fixture.frame(1);
    fixture
        .pane
        .update_context(PaneContextFacts {
            focused_pane: false,
            terminal_input_focus: false,
            presentable: true,
        })
        .unwrap();
    let frame = fixture.pane.current_frame().unwrap();
    assert!(frame.is_paintable());
    assert!(!frame.permits_native_content_use());
}

fn graphics_request(fixture: &Fixture, stamp: QueryStamp, digest: [u8; 32]) -> GraphicsRequest {
    let resource = GraphicsResourceRef {
        key: GraphicsResourceKey {
            session: stamp.base().session,
            id: GraphicsResourceId::new(1).unwrap(),
            generation: 1,
        },
        width: 2,
        height: 1,
        rgba_bytes: 8,
        digest,
    };
    let mut parts = page(stamp.base(), 1, false).into_parts();
    parts.resources.push(resource);
    fixture
        .pane
        .projection()
        .lock()
        .unwrap()
        .accept_page(stamp, ContentPage::new(parts).unwrap())
        .unwrap();
    GraphicsRequest::new(stamp, ResourceTransferId::new(1).unwrap(), resource, 0).unwrap()
}

#[tokio::test]
async fn complete_graphics_assets_have_separate_external_residency_and_frozen_paint() {
    let graphics = Residency::new(8, 8).unwrap();
    let semantic = Residency::new(16384, 16384).unwrap();
    let fixture = Fixture::with_budgets(
        true,
        ProjectionBudgets {
            semantic: semantic.clone(),
            prepared_copy: Residency::new(16, 16).unwrap(),
            graphics: graphics.clone(),
        },
    )
    .await;
    let stamp = fixture.frame(1);
    let bytes = vec![1, 2, 3, 4, 5, 6, 7, 8];
    let request = graphics_request(&fixture, stamp, Sha256::digest(&bytes).into());
    let projection = fixture.pane.projection();
    let mut preparation = projection.lock().unwrap().begin_graphics(request).unwrap();
    assert!(
        preparation
            .accept(GraphicsChunk::new(request, 0, bytes[..4].to_vec(), false).unwrap())
            .unwrap()
            .is_none()
    );
    assert!(
        projection
            .lock()
            .unwrap()
            .graphics(request.resource().key)
            .is_none()
    );
    let asset = preparation
        .accept(GraphicsChunk::new(request, 4, bytes[4..].to_vec(), true).unwrap())
        .unwrap()
        .unwrap();
    projection
        .lock()
        .unwrap()
        .install_graphics(asset.clone())
        .unwrap();
    assert_eq!(asset.rgba(), Some(bytes.as_slice()));
    assert_eq!(graphics.held_bytes().unwrap(), 8);
    assert!(semantic.held_bytes().unwrap() < 16384);
    fixture
        .channels
        .invalidate(
            fixture.server.server(),
            ConnectionInvalidation::ConnectionLost,
        )
        .unwrap();
    assert!(asset.is_paintable());
    fixture.takeover().await;
    assert!(asset.rgba().is_none());
    assert_eq!(graphics.held_bytes().unwrap(), 8);
    drop(asset);
    assert_eq!(graphics.held_bytes().unwrap(), 0);
}

#[tokio::test]
async fn wrong_graphics_offset_digest_transfer_and_capacity_never_install_an_asset() {
    for scenario in 0..4 {
        let graphics = Residency::new(if scenario == 3 { 7 } else { 8 }, 8).unwrap();
        let fixture = Fixture::with_budgets(
            true,
            ProjectionBudgets {
                semantic: Residency::new(16384, 16384).unwrap(),
                prepared_copy: Residency::new(16, 16).unwrap(),
                graphics: graphics.clone(),
            },
        )
        .await;
        let stamp = fixture.frame(1);
        let bytes = vec![1; 8];
        let request = graphics_request(
            &fixture,
            stamp,
            if scenario == 1 {
                [0; 32]
            } else {
                Sha256::digest(&bytes).into()
            },
        );
        let projection = fixture.pane.projection();
        let mut preparation = projection.lock().unwrap().begin_graphics(request).unwrap();
        let chunk_request = if scenario == 2 {
            GraphicsRequest::new(
                stamp,
                ResourceTransferId::new(2).unwrap(),
                request.resource(),
                0,
            )
            .unwrap()
        } else {
            request
        };
        let (offset, data) = if scenario == 0 {
            (4, vec![1; 4])
        } else {
            (0, bytes)
        };
        assert!(
            preparation
                .accept(GraphicsChunk::new(chunk_request, offset, data, true).unwrap())
                .is_err()
        );
        assert!(
            projection
                .lock()
                .unwrap()
                .graphics(request.resource().key)
                .is_none()
        );
        assert_eq!(graphics.held_bytes().unwrap(), 0);
    }
}

#[tokio::test]
async fn takeover_discards_graphics_preparation_even_when_its_final_chunk_is_valid() {
    let fixture = Fixture::new(true).await;
    let stamp = fixture.frame(1);
    let bytes = vec![1; 8];
    let request = graphics_request(&fixture, stamp, Sha256::digest(&bytes).into());
    let mut preparation = fixture
        .pane
        .projection()
        .lock()
        .unwrap()
        .begin_graphics(request)
        .unwrap();
    fixture.takeover().await;
    assert!(
        preparation
            .accept(GraphicsChunk::new(request, 0, bytes, true).unwrap())
            .is_err()
    );
}

#[tokio::test]
async fn find_rejects_old_authority_even_when_a_query_uses_the_current_local_generation() {
    let fixture = Fixture::new(true).await;
    let stamp = fixture.frame(1);
    fixture
        .pane
        .update_context(PaneContextFacts {
            focused_pane: true,
            terminal_input_focus: false,
            presentable: true,
        })
        .unwrap();
    let projection = fixture.pane.projection();
    let mut projection = projection.lock().unwrap();
    let stale_stamp = QueryStamp::new(
        stamp.fence(),
        stamp.base(),
        stamp.request(),
        projection.find().generation(),
    )
    .unwrap();
    let request = FindRequest::new(
        stale_stamp,
        BoundedFindQuery::new("query".into()).unwrap(),
        None,
    )
    .unwrap();
    assert_eq!(
        projection
            .find_mut()
            .unwrap()
            .begin(request)
            .unwrap_err()
            .class(),
        ClientFailureClass::StaleAuthority
    );
}

#[tokio::test]
async fn different_read_requests_cannot_splice_pages_into_one_frame() {
    let fixture = Fixture::new(true).await;
    let stamp = fixture.frame(1);
    let projection = fixture.pane.projection();
    projection
        .lock()
        .unwrap()
        .accept_page(stamp, page(stamp.base(), 1, true))
        .unwrap();
    let permit = fixture.pane.permit(EffectKind::Read).unwrap();
    let different = fixture
        .pane
        .query_stamp(&permit, stamp.generation())
        .unwrap();
    assert!(
        projection
            .lock()
            .unwrap()
            .accept_page(different, page(stamp.base(), 2, false))
            .is_err()
    );
    assert!(projection.lock().unwrap().needs_content_resync());
}

#[tokio::test]
async fn a_new_page_read_revokes_older_page_completion_before_the_new_result_arrives() {
    let fixture = Fixture::new(true).await;
    let old = fixture.frame(1);
    let permit = fixture.pane.permit(EffectKind::Read).unwrap();
    let stamp = fixture.pane.query_stamp(&permit, old.generation()).unwrap();
    let request = ContentPageRequest::new(
        stamp,
        ContentRange::ActiveGrid,
        None,
        PageLimits::new(256, 16384, 1024 * 1024).unwrap(),
    )
    .unwrap();
    fixture
        .peer
        .queue_read(vec![PaneReadItem::Page(ContentReadResult::Ready(
            ContentPageReply::new(&request, page(stamp.base(), 2, true)).unwrap(),
        ))])
        .unwrap();
    let mut stream = fixture
        .pane
        .read(&permit, PaneReadRequest::Page(request))
        .await
        .unwrap();
    let projection = fixture.pane.projection();
    assert_eq!(
        projection
            .lock()
            .unwrap()
            .accept_page(old, page(old.base(), 1, false))
            .unwrap_err()
            .class(),
        ClientFailureClass::StaleAuthority
    );
    let Some(PaneReadItem::Page(ContentReadResult::Ready(reply))) = stream.next().await.unwrap()
    else {
        panic!("new page required")
    };
    projection
        .lock()
        .unwrap()
        .accept_page(reply.stamp(), reply.page().clone())
        .unwrap();
    assert_eq!(
        projection
            .lock()
            .unwrap()
            .accept_page(old, page(old.base(), 1, false))
            .unwrap_err()
            .class(),
        ClientFailureClass::StaleAuthority
    );
    assert!(
        !projection.lock().unwrap().needs_content_resync(),
        "an old result must not invalidate newer staged pages"
    );
    projection
        .lock()
        .unwrap()
        .accept_page(stamp, page(stamp.base(), 3, false))
        .unwrap();
    assert_eq!(
        projection
            .lock()
            .unwrap()
            .current_frame()
            .unwrap()
            .frame()
            .unwrap()
            .pages()[0]
            .page()
            .parts()
            .rows[0]
            .id(),
        row(stamp.base(), 2)
    );
}

fn fragmented_page(base: ContentBase, start: u16, end: u16) -> ContentPage {
    let mut parts = ContentPageParts::new(base, 6);
    let cells = (start..end)
        .map(|_| {
            StyledCell::new(
                CellContent::grapheme("x".into(), CellWidth::Single).unwrap(),
                CellStyle::default(),
                None,
            )
        })
        .collect();
    let fragment = ContentRow::fragment(row(base, 1), 1, false, 6, start, cells).unwrap();
    if end < 6 {
        parts.continuation = Some(PageContinuation::new(base, &fragment).unwrap());
    }
    parts.rows.push(fragment);
    ContentPage::new(parts).unwrap()
}

#[tokio::test]
async fn orphaned_initial_row_suffix_requires_resync_and_preserves_display() {
    let fixture = Fixture::new(true).await;
    let stamp = fixture.frame(1);
    let projection = fixture.pane.projection();
    let mut projection = projection.lock().unwrap();
    assert_eq!(
        projection
            .accept_page(stamp, fragmented_page(stamp.base(), 4, 6))
            .unwrap_err()
            .class(),
        ClientFailureClass::InvalidPeer,
    );
    assert!(projection.needs_content_resync());
    assert_eq!(
        projection.current_frame().unwrap().frame().unwrap().pages()[0]
            .page()
            .parts()
            .columns,
        2
    );
}

#[tokio::test]
async fn complete_physical_row_is_assembled_across_bounded_pages() {
    let fixture = Fixture::new(true).await;
    let stamp = fixture.frame(1);
    let projection = fixture.pane.projection();
    let mut projection = projection.lock().unwrap();
    for (start, end) in [(0, 2), (2, 4)] {
        assert!(
            !projection
                .accept_page(stamp, fragmented_page(stamp.base(), start, end))
                .unwrap()
        );
        assert_eq!(
            projection.current_frame().unwrap().frame().unwrap().pages()[0]
                .page()
                .parts()
                .columns,
            2
        );
    }
    assert!(
        projection
            .accept_page(stamp, fragmented_page(stamp.base(), 4, 6))
            .unwrap()
    );
    let frame = projection.current_frame().unwrap();
    let fragments = frame.frame().unwrap().pages();
    assert_eq!(fragments.len(), 3);
    assert_eq!(
        fragments
            .iter()
            .map(|page| page.page().parts().rows[0].cells().len())
            .sum::<usize>(),
        6
    );
    assert!(!projection.needs_content_resync());
}

#[tokio::test]
async fn visible_unfocused_split_pane_installs_new_semantic_content_and_graphics() {
    use spaceterm_core::{
        operations::Command,
        organization::{SplitAxis, SplitEdge},
    };
    let fixture = Fixture::new(true).await;
    let old_stamp = fixture.frame(1);
    let tab = fixture.server.snapshot().unwrap().state().panes[0].tab;
    fixture
        .peer
        .submit(
            Command::CreatePane {
                tab,
                source: fixture.pane.pane(),
                axis: SplitAxis::Horizontal,
                edge: SplitEdge::After,
            },
            Some(fixture.workspace),
        )
        .await
        .unwrap();
    let id = fixture
        .server
        .snapshot()
        .unwrap()
        .state()
        .panes
        .iter()
        .find(|pane| pane.id != fixture.pane.pane())
        .unwrap()
        .id;
    let focused = fixture.lease.pane(id).unwrap();
    focused
        .update_context(PaneContextFacts {
            focused_pane: true,
            terminal_input_focus: true,
            presentable: true,
        })
        .unwrap();
    let intent = focused
        .explicit_use(ExplicitUseSource::ExplicitSelection)
        .await
        .unwrap()
        .startup()
        .unwrap();
    focused.complete_intent(&intent, grid()).await.unwrap();
    fixture
        .pane
        .update_context(PaneContextFacts {
            focused_pane: false,
            terminal_input_focus: false,
            presentable: true,
        })
        .unwrap();
    let fence = fixture.pane.permit(EffectKind::Resize).unwrap().fence();
    let base = ContentBase {
        revision: ContentRevision::new(2),
        ..old_stamp.base()
    };
    fixture.pane.publish_content_base(fence, base).unwrap();
    let permit = fixture
        .pane
        .permit(EffectKind::Read)
        .expect("a visible controlled split Pane can read while unfocused");
    let stamp = fixture
        .pane
        .query_stamp(&permit, RequestGeneration::new(1))
        .unwrap();
    let bytes = vec![1; 8];
    let request = graphics_request(&fixture, stamp, Sha256::digest(&bytes).into());
    let page_request = ContentPageRequest::new(
        stamp,
        ContentRange::ActiveGrid,
        None,
        PageLimits::new(256, 16384, 1024 * 1024).unwrap(),
    )
    .unwrap();
    fixture
        .peer
        .queue_read(vec![PaneReadItem::Page(ContentReadResult::Ready(
            ContentPageReply::new(&page_request, page(base, 1, false)).unwrap(),
        ))])
        .unwrap();
    let mut read = fixture
        .pane
        .read(&permit, PaneReadRequest::Page(page_request))
        .await
        .unwrap();
    assert!(read.next().await.unwrap().is_some());
    let projection = fixture.pane.projection();
    let mut preparation = projection.lock().unwrap().begin_graphics(request).unwrap();
    let asset = preparation
        .accept(GraphicsChunk::new(request, 0, bytes.clone(), true).unwrap())
        .unwrap()
        .unwrap();
    projection.lock().unwrap().install_graphics(asset).unwrap();
    let frame = fixture.pane.current_frame().unwrap();
    assert_eq!(frame.frame().unwrap().base(), base);
    assert!(!frame.permits_native_content_use());
    assert_eq!(
        projection
            .lock()
            .unwrap()
            .graphics(request.resource().key)
            .unwrap()
            .rgba(),
        Some(bytes.as_slice())
    );
    assert!(fixture.pane.permit(EffectKind::ImeCommit).is_err());
    assert!(fixture.pane.permit(EffectKind::NativeCompletion).is_err());
    assert!(
        projection
            .lock()
            .unwrap()
            .set_selection(Some(selection(stamp)))
            .is_err()
    );
}

#[tokio::test]
async fn announced_content_revision_preserves_display_through_disconnect_or_allocation_failure() {
    for disconnect in [true, false] {
        let fixture = Fixture::with_budgets(
            true,
            ProjectionBudgets {
                semantic: Residency::new(2 * 4364, 2 * 4364).unwrap(),
                prepared_copy: Residency::new(16, 16).unwrap(),
                graphics: Residency::new(16, 16).unwrap(),
            },
        )
        .await;
        let old_stamp = fixture.frame(1);
        let displayed = fixture.pane.current_frame().unwrap();
        let base = ContentBase {
            revision: ContentRevision::new(2),
            ..old_stamp.base()
        };
        fixture
            .pane
            .publish_content_base(fixture.pane.permit(EffectKind::Key).unwrap().fence(), base)
            .unwrap();
        assert!(
            displayed.is_paintable(),
            "a base announcement is not a replacement screen"
        );
        assert!(!displayed.permits_native_content_use());
        let stamp = fixture
            .pane
            .query_stamp(
                &fixture.pane.permit(EffectKind::Read).unwrap(),
                RequestGeneration::new(1),
            )
            .unwrap();
        let projection = fixture.pane.projection();
        assert!(
            !projection
                .lock()
                .unwrap()
                .accept_page(stamp, page(base, 1, true))
                .unwrap()
        );
        if disconnect {
            fixture
                .channels
                .invalidate(
                    fixture.server.server(),
                    ConnectionInvalidation::ConnectionLost,
                )
                .unwrap();
        } else {
            assert_eq!(
                projection
                    .lock()
                    .unwrap()
                    .accept_page(stamp, page(base, 2, false))
                    .unwrap_err()
                    .class(),
                ClientFailureClass::ResourceLimit
            );
        }
        let current = fixture
            .pane
            .current_frame()
            .expect("the complete previous screen must survive failed replacement");
        assert_eq!(current.frame().unwrap().base(), old_stamp.base());
        assert_eq!(current.frozen(), disconnect);
        assert!(displayed.is_paintable());
        assert!(!current.permits_native_content_use());
    }
}

#[tokio::test]
async fn complete_new_revision_replaces_the_display_atomically() {
    let fixture = Fixture::new(true).await;
    let old = fixture.frame(1);
    let displayed = fixture.pane.current_frame().unwrap();
    let base = ContentBase {
        revision: ContentRevision::new(2),
        ..old.base()
    };
    fixture
        .pane
        .publish_content_base(fixture.pane.permit(EffectKind::Key).unwrap().fence(), base)
        .unwrap();
    let stamp = fixture
        .pane
        .query_stamp(
            &fixture.pane.permit(EffectKind::Read).unwrap(),
            RequestGeneration::new(1),
        )
        .unwrap();
    let projection = fixture.pane.projection();
    assert!(
        !projection
            .lock()
            .unwrap()
            .accept_page(stamp, page(base, 1, true))
            .unwrap()
    );
    assert!(displayed.is_paintable());
    assert!(
        projection
            .lock()
            .unwrap()
            .accept_page(stamp, page(base, 2, false))
            .unwrap()
    );
    assert_eq!(
        fixture
            .pane
            .current_frame()
            .unwrap()
            .frame()
            .unwrap()
            .base(),
        base
    );
    assert!(!displayed.is_paintable());
}
