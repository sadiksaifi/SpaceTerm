#![cfg(feature = "test-support")]
#[path = "projection/support.rs"]
mod support;
use spaceterm_client::{
    failure::ClientFailureClass, pane_channel::*, ports::ConnectionInvalidation,
};
use spaceterm_core::{content_queries::*, native_requests::*, observations::MonotonicTick};
use std::time::Duration;
use support::*;

#[tokio::test]
async fn resource_chunk_and_transfer_count_limits_are_enforced() {
    let fixture = Fixture::new(true).await;
    let stamp = fixture.frame(1);
    let permit = fixture.pane.permit(EffectKind::Read).unwrap();
    let request = ContentPageRequest::new(
        stamp,
        ContentRange::ActiveGrid,
        None,
        PageLimits::new(256, 16384, 1024 * 1024).unwrap(),
    )
    .unwrap();
    let item = PaneReadItem::Page(ContentReadResult::Ready(
        ContentPageReply::new(&request, page(stamp.base(), 1, false)).unwrap(),
    ));
    let mut streams = vec![];
    for _ in 0..4 {
        fixture.peer.queue_read(vec![item.clone()]).unwrap();
        streams.push(
            fixture
                .pane
                .read(&permit, PaneReadRequest::Page(request))
                .await
                .unwrap(),
        );
    }
    fixture.peer.queue_read(vec![item]).unwrap();
    let error = match fixture
        .pane
        .read(&permit, PaneReadRequest::Page(request))
        .await
    {
        Err(error) => error,
        Ok(_) => panic!("fifth transfer admitted"),
    };
    assert_eq!(error.class(), ClientFailureClass::ResourceLimit);
    drop(streams.pop());
    fixture
        .pane
        .read(&permit, PaneReadRequest::Page(request))
        .await
        .unwrap();
    let copy = copy_request(stamp, false);
    assert!(
        CopyChunk::new(
            &copy,
            ResourceTransferId::new(1).unwrap(),
            CopyPart::Plain,
            0,
            (MAX_RESOURCE_CHUNK_BYTES + 1) as u64,
            vec![b'x'; MAX_RESOURCE_CHUNK_BYTES + 1]
        )
        .is_err()
    );
}
#[tokio::test]
async fn slow_subscriber_is_invalidated_without_stalling_reconciliation() {
    let fixture = Fixture::new(true).await;
    let mut events = fixture.pane.events().unwrap();
    for _ in 0..65 {
        fixture
            .channels
            .invalidate(fixture.server.server(), ConnectionInvalidation::Publication)
            .unwrap();
    }
    assert_eq!(
        events.next().await.unwrap_err().class(),
        ClientFailureClass::ResourceLimit
    );
    fixture.peer.reconnect(2, 2).unwrap();
    assert!(fixture.pane.permit(EffectKind::ImeCommit).is_err());
    fixture.frame(1);
    assert!(fixture.pane.permit(EffectKind::ImeCommit).is_ok());
    fixture.pane.events().unwrap();
}
#[tokio::test(start_paused = true)]
async fn native_request_deadlines_and_single_pending_paste_are_not_extended() {
    let fixture = Fixture::new(true).await;
    fixture.frame(1);
    let permit = fixture.pane.permit(EffectKind::NativeCompletion).unwrap();
    let request = NativeRequest::new(
        permit.fence(),
        NativeRequestId::new(1).unwrap(),
        MonotonicTick::new(0),
        MonotonicTick::new(0),
        Some(MonotonicTick::new(30000)),
        NativeRequestKind::UnsafePaste {
            approval: PasteApprovalId::new(1).unwrap(),
            text: NativeText::new("paste".into()).unwrap(),
        },
    )
    .unwrap();
    fixture
        .pane
        .receive_native(
            request.clone(),
            NativeTiming {
                not_before: Duration::ZERO,
                expires_after: Some(Duration::from_secs(30)),
            },
        )
        .unwrap();
    let second = NativeRequest::new(
        permit.fence(),
        NativeRequestId::new(2).unwrap(),
        MonotonicTick::new(0),
        MonotonicTick::new(0),
        Some(MonotonicTick::new(30000)),
        request.kind().clone(),
    )
    .unwrap();
    assert_eq!(
        fixture
            .pane
            .receive_native(
                second,
                NativeTiming {
                    not_before: Duration::ZERO,
                    expires_after: Some(Duration::from_secs(30))
                }
            )
            .unwrap_err()
            .class(),
        ClientFailureClass::ResourceLimit
    );
    tokio::time::advance(Duration::from_secs(30)).await;
    assert!(
        fixture
            .pane
            .complete_native(
                &permit,
                NativeCompletion::new(
                    &request,
                    NativeCompletionKind::Paste {
                        approval: PasteApprovalId::new(1).unwrap(),
                        decision: PasteDecision::Confirm
                    }
                )
                .unwrap()
            )
            .is_err()
    );
}
#[tokio::test]
async fn negotiated_resize_limit_is_checked_before_queue_admission() {
    let fixture = Fixture::new(true).await;
    *fixture.peer.grid_limits.lock().unwrap() =
        spaceterm_core::geometry::GridLimits::new(24, 80, 24 * 80).unwrap();
    let permit = fixture.pane.permit(EffectKind::Resize).unwrap();
    assert_eq!(
        fixture
            .pane
            .try_effect(&permit, RuntimeEffect::Resize(grid().into()))
            .unwrap_err()
            .class(),
        ClientFailureClass::ResourceLimit
    );
    assert!(fixture.peer.flush_effects().unwrap().is_empty());
}

#[tokio::test]
async fn failed_resource_stream_cannot_resume_into_a_copy_completion() {
    let fixture = Fixture::new(true).await;
    let stamp = fixture.frame(1);
    let permit = fixture.pane.permit(EffectKind::Read).unwrap();
    let request = copy_request(stamp, false);
    let transfer = ResourceTransferId::new(1).unwrap();
    fixture
        .peer
        .queue_read_results(vec![
            Ok(PaneReadItem::Copy(CopyStreamItem::Chunk(
                CopyChunk::new(&request, transfer, CopyPart::Plain, 0, 1, vec![b'x']).unwrap(),
            ))),
            Err(spaceterm_client::failure::ClientFailure::new(
                ClientFailureClass::ResourceLimit,
            )),
            Ok(PaneReadItem::Copy(CopyStreamItem::Complete(
                CopyCompletion::new(&request, transfer, 1, [0; 32], None).unwrap(),
            ))),
        ])
        .unwrap();
    let mut stream = fixture
        .pane
        .read(&permit, PaneReadRequest::Copy(request))
        .await
        .unwrap();
    stream.next().await.unwrap().unwrap();
    assert_eq!(
        stream.next().await.unwrap_err().class(),
        ClientFailureClass::ResourceLimit
    );
    assert!(
        stream.next().await.unwrap().is_none(),
        "source failure terminates the whole transfer"
    );
}

#[tokio::test]
async fn injected_projection_budgets_can_only_reduce_supported_limits() {
    use spaceterm_client::{
        projection::{ProjectionBudgets, budget::Residency},
        testing::{FakePaneConnection, FakeServer},
    };
    let server = FakeServer::new(1, vec![2, 1]).unwrap();
    let peer = FakePaneConnection::new(server, 1, 2).await.unwrap();
    let mut budgets = ProjectionBudgets::supported().unwrap();
    budgets.semantic = Residency::new(17 * 1024 * 1024, 64 * 1024 * 1024).unwrap();
    match peer.channels(budgets) {
        Err(error) => assert_eq!(error.class(), ClientFailureClass::ResourceLimit),
        Ok(_) => panic!("injected limits increased the supported semantic budget"),
    }
}

#[tokio::test]
async fn native_request_residency_is_bounded_by_bytes_not_only_envelopes() {
    let fixture = Fixture::new(true).await;
    let permit = fixture.pane.permit(EffectKind::NativeCompletion).unwrap();
    for id in 1..=4 {
        let request = NativeRequest::new(
            permit.fence(),
            NativeRequestId::new(id).unwrap(),
            MonotonicTick::new(0),
            MonotonicTick::new(0),
            Some(MonotonicTick::new(1000)),
            NativeRequestKind::ClipboardWrite(
                NativeText::new("x".repeat(spaceterm_protocol::limits::MAX_CLIPBOARD_BYTES))
                    .unwrap(),
            ),
        )
        .unwrap();
        let result = fixture.pane.receive_native(
            request,
            NativeTiming {
                not_before: Duration::ZERO,
                expires_after: Some(Duration::from_secs(1)),
            },
        );
        if id < 4 {
            result.unwrap();
        } else {
            assert_eq!(
                result.unwrap_err().class(),
                ClientFailureClass::ResourceLimit
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn native_completion_deadline_is_rechecked_at_final_dequeue() {
    let fixture = Fixture::new(true).await;
    let permit = fixture.pane.permit(EffectKind::NativeCompletion).unwrap();
    let request = NativeRequest::new(
        permit.fence(),
        NativeRequestId::new(1).unwrap(),
        MonotonicTick::new(0),
        MonotonicTick::new(0),
        Some(MonotonicTick::new(1000)),
        NativeRequestKind::ClipboardWrite(NativeText::new("text".into()).unwrap()),
    )
    .unwrap();
    fixture
        .pane
        .receive_native(
            request.clone(),
            NativeTiming {
                not_before: Duration::ZERO,
                expires_after: Some(Duration::from_secs(1)),
            },
        )
        .unwrap();
    fixture
        .pane
        .complete_native(
            &permit,
            NativeCompletion::new(&request, NativeCompletionKind::ClipboardWritten).unwrap(),
        )
        .unwrap();
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(
        fixture.peer.flush_effects().unwrap().is_empty(),
        "queued completion expired before transmission"
    );
}

#[tokio::test]
async fn retained_native_request_leases_remain_charged_after_context_invalidation() {
    let fixture = Fixture::new(true).await;
    let permit = fixture.pane.permit(EffectKind::NativeCompletion).unwrap();
    let mut events = fixture.pane.events().unwrap();
    let timing = NativeTiming {
        not_before: Duration::ZERO,
        expires_after: Some(Duration::from_secs(1)),
    };
    let request = |id, fence| {
        NativeRequest::new(
            fence,
            NativeRequestId::new(id).unwrap(),
            MonotonicTick::new(0),
            MonotonicTick::new(0),
            Some(MonotonicTick::new(1000)),
            NativeRequestKind::ClipboardWrite(
                NativeText::new("x".repeat(spaceterm_protocol::limits::MAX_CLIPBOARD_BYTES))
                    .unwrap(),
            ),
        )
        .unwrap()
    };
    let mut held = vec![];
    for id in 1..=3 {
        fixture
            .pane
            .receive_native(request(id, permit.fence()), timing)
            .unwrap();
        let PaneEvent::Native { request, .. } = events.next().await.unwrap().unwrap() else {
            panic!("native event required")
        };
        assert!(request.is_current());
        held.push(request);
    }
    fixture
        .pane
        .update_context(PaneContextFacts {
            focused_pane: true,
            terminal_input_focus: false,
            presentable: true,
        })
        .unwrap();
    assert!(held.iter().all(|request| !request.is_current()));
    fixture
        .pane
        .update_context(PaneContextFacts {
            focused_pane: true,
            terminal_input_focus: true,
            presentable: true,
        })
        .unwrap();
    let fence = fixture
        .pane
        .permit(EffectKind::NativeCompletion)
        .unwrap()
        .fence();
    assert_eq!(
        fixture
            .pane
            .receive_native(request(4, fence), timing)
            .unwrap_err()
            .class(),
        ClientFailureClass::ResourceLimit
    );
    drop(held);
    fixture
        .pane
        .receive_native(request(4, fence), timing)
        .unwrap();
}
