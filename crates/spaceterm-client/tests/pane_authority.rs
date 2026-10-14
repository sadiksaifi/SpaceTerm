#![cfg(feature = "test-support")]
#[path = "projection/support.rs"]
mod support;
use spaceterm_client::{
    failure::ClientFailureClass, pane_channel::*, ports::ConnectionInvalidation,
};
use spaceterm_core::{
    content_queries::*, native_requests::*, observations::MonotonicTick, organization::SessionPhase,
};
use std::time::Duration;
use support::*;

#[tokio::test]
async fn measurement_without_explicit_intent_sends_no_start() {
    let fixture = Fixture::new(false).await;
    fixture
        .pane
        .update_context(PaneContextFacts {
            focused_pane: true,
            terminal_input_focus: false,
            presentable: true,
        })
        .unwrap();
    fixture.peer.publish().unwrap();
    assert!(matches!(
        fixture.pane.state().borrow().runtime.unwrap().phase,
        SessionPhase::NotStarted
    ));
    assert_eq!(fixture.server.received_operations().unwrap().len(), 1);
}
#[tokio::test]
async fn intent_completes_once_at_measured_dimensions() {
    let fixture = Fixture::new(false).await;
    let token = fixture
        .pane
        .explicit_use(ExplicitUseSource::StartSession)
        .await
        .unwrap()
        .startup()
        .unwrap();
    let ticket = fixture.pane.complete_intent(&token, grid()).await.unwrap();
    let operation = fixture
        .peer
        .journal()
        .snapshot()
        .unwrap()
        .operation(ticket.id())
        .unwrap()
        .clone();
    let request = spaceterm_core::operation_ledger::CanonicalOperationRequest::decode(
        &operation.request,
        fixture.peer.journal().codec(),
    )
    .unwrap();
    assert!(
        matches!(request.command(), spaceterm_core::operations::Command::CompleteMeasuredStartup(measurement) if measurement.grid == grid().into() && measurement.fence == token.intent().fence())
    );
    assert!(fixture.pane.complete_intent(&token, grid()).await.is_err());
}
#[tokio::test]
async fn cancelled_or_taken_over_intent_cannot_complete() {
    for takeover in [false, true] {
        let fixture = Fixture::new(false).await;
        let intent = fixture
            .pane
            .explicit_use(ExplicitUseSource::ExplicitSelection)
            .await
            .unwrap()
            .startup()
            .unwrap();
        if takeover {
            fixture.takeover().await;
        } else {
            fixture.pane.cancel_intent(&intent);
        }
        assert!(fixture.pane.complete_intent(&intent, grid()).await.is_err());
    }
}
#[tokio::test]
async fn ime_commit_preserves_focused_pane_exception() {
    let fixture = Fixture::new(true).await;
    fixture
        .pane
        .update_context(PaneContextFacts {
            focused_pane: true,
            terminal_input_focus: false,
            presentable: true,
        })
        .unwrap();
    assert!(fixture.pane.permit(EffectKind::Key).is_err());
    let permit = fixture.pane.permit(EffectKind::ImeCommit).unwrap();
    fixture
        .pane
        .try_effect(
            &permit,
            RuntimeEffect::InputMethodCommit(
                spaceterm_terminal::runtime_contracts::BoundedInputMethodText::new("text".into())
                    .unwrap(),
            ),
        )
        .unwrap();
    assert_eq!(fixture.peer.flush_effects().unwrap().len(), 1);
}
#[tokio::test]
async fn disconnect_discards_input_and_never_replays_old_queue() {
    let fixture = Fixture::new(true).await;
    let permit = fixture.pane.permit(EffectKind::ImeCommit).unwrap();
    let input = RuntimeEffect::InputMethodCommit(
        spaceterm_terminal::runtime_contracts::BoundedInputMethodText::new("text".into()).unwrap(),
    );
    fixture.pane.try_effect(&permit, input.clone()).unwrap();
    fixture
        .channels
        .invalidate(
            fixture.server.server(),
            ConnectionInvalidation::ConnectionLost,
        )
        .unwrap();
    assert_eq!(
        fixture.pane.try_effect(&permit, input).unwrap_err().class(),
        ClientFailureClass::StaleAuthority
    );
    fixture.peer.reconnect(2, 2).unwrap();
    assert!(!permit.is_current());
    assert!(fixture.peer.flush_effects().unwrap().is_empty());
}
#[tokio::test]
async fn takeover_revokes_old_input_resize_and_native_completion() {
    let fixture = Fixture::new(true).await;
    let stamp = fixture.frame(1);
    let ime = fixture.pane.permit(EffectKind::ImeCommit).unwrap();
    let resize = fixture.pane.permit(EffectKind::Resize).unwrap();
    let native = fixture.pane.permit(EffectKind::NativeCompletion).unwrap();
    let request = NativeRequest::new(
        native.fence(),
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
                expires_after: Some(Duration::from_millis(1000)),
            },
        )
        .unwrap();
    fixture.takeover().await;
    assert!(!ime.is_current());
    assert!(!resize.is_current());
    assert!(!native.is_current());
    assert!(
        fixture
            .pane
            .complete_native(
                &native,
                NativeCompletion::new(&request, NativeCompletionKind::ClipboardWritten).unwrap()
            )
            .is_err()
    );
    assert!(
        fixture
            .pane
            .projection()
            .lock()
            .unwrap()
            .accept_page(stamp, page(stamp.base(), 1, false))
            .is_err()
    );
}
#[tokio::test]
async fn offline_keeps_last_eligible_frame_frozen_and_reconciles_before_live_use() {
    let fixture = Fixture::new(true).await;
    fixture.frame(1);
    let old = fixture.pane.current_frame().unwrap();
    fixture
        .channels
        .invalidate(
            fixture.server.server(),
            ConnectionInvalidation::ConnectionLost,
        )
        .unwrap();
    assert!(old.is_paintable());
    assert!(old.frozen());
    assert!(!old.permits_native_content_use());
    fixture.peer.reconnect(2, 2).unwrap();
    let current = fixture
        .pane
        .current_frame()
        .expect("the same reconciled Session keeps its displayed frame");
    assert!(current.is_paintable());
    assert!(current.frozen());
    assert!(!current.permits_native_content_use());
    fixture.frame(1);
    assert!(!fixture.pane.current_frame().unwrap().frozen());
    assert!(!old.permits_native_content_use());
}
#[tokio::test]
async fn offline_takeover_never_reveals_cached_terminal_content() {
    let fixture = Fixture::new(true).await;
    fixture.frame(1);
    let old = fixture.pane.current_frame().unwrap();
    fixture.takeover().await;
    fixture
        .channels
        .invalidate(
            fixture.server.server(),
            ConnectionInvalidation::ConnectionLost,
        )
        .unwrap();
    assert!(!old.is_paintable());
    assert!(fixture.pane.current_frame().is_none());
    assert_eq!(
        fixture.pane.state().borrow().readiness,
        PaneReadiness::Takeover
    );
}
#[tokio::test]
async fn old_connection_page_cannot_update_reconnected_pane() {
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
    let reply = ContentPageReply::new(&request, page(stamp.base(), 1, false)).unwrap();
    fixture
        .peer
        .queue_read(vec![PaneReadItem::Page(ContentReadResult::Ready(reply))])
        .unwrap();
    let mut stream = fixture
        .pane
        .read(&permit, PaneReadRequest::Page(request))
        .await
        .unwrap();
    fixture
        .channels
        .invalidate(
            fixture.server.server(),
            ConnectionInvalidation::ConnectionLost,
        )
        .unwrap();
    fixture.peer.reconnect(2, 2).unwrap();
    assert_eq!(
        stream.next().await.unwrap_err().class(),
        ClientFailureClass::StaleAuthority
    );
}
#[tokio::test]
async fn dropping_channels_and_subscribers_sends_no_shutdown_or_release() {
    let fixture = Fixture::new(true).await;
    let before = fixture.server.received_operations().unwrap().len();
    let subscriber = fixture.pane.events().unwrap();
    drop(subscriber);
    drop(fixture.pane.clone());
    assert_eq!(fixture.server.received_operations().unwrap().len(), before);
    assert!(matches!(
        fixture.server.snapshot().unwrap().runtime()[0].phase,
        SessionPhase::Running { .. }
    ));
}

#[tokio::test]
async fn delayed_old_intent_response_cannot_cancel_a_new_context_request() {
    let fixture = Fixture::new(false).await;
    let old_gate = fixture.peer.delay_next_intent_response().unwrap();
    let pane = fixture.pane.clone();
    let old = tokio::spawn(async move { pane.explicit_use(ExplicitUseSource::StartSession).await });
    wait_for_operations(&fixture, 2).await;
    fixture
        .pane
        .update_context(PaneContextFacts {
            focused_pane: true,
            terminal_input_focus: false,
            presentable: true,
        })
        .unwrap();
    let new_gate = fixture.peer.delay_next_intent_response().unwrap();
    let pane = fixture.pane.clone();
    let new = tokio::spawn(async move { pane.explicit_use(ExplicitUseSource::StartSession).await });
    wait_for_operations(&fixture, 3).await;
    old_gate.add_permits(1);
    assert!(old.await.unwrap().is_err());
    new_gate.add_permits(1);
    let token = new
        .await
        .unwrap()
        .expect("the old response must not consume the new pending request")
        .startup()
        .unwrap();
    fixture.pane.complete_intent(&token, grid()).await.unwrap();
    assert_eq!(fixture.peer.cancelled_intents().unwrap().len(), 1);
}

async fn wait_for_operations(fixture: &Fixture, count: usize) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while fixture.server.received_operations().unwrap().len() < count {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn native_request_identity_is_not_reused_when_control_returns_to_the_same_session() {
    let fixture = Fixture::new(true).await;
    let request = |fence| {
        NativeRequest::new(
            fence,
            NativeRequestId::new(1).unwrap(),
            MonotonicTick::new(0),
            MonotonicTick::new(0),
            Some(MonotonicTick::new(1000)),
            NativeRequestKind::ClipboardWrite(NativeText::new("text".into()).unwrap()),
        )
        .unwrap()
    };
    let timing = NativeTiming {
        not_before: Duration::ZERO,
        expires_after: Some(Duration::from_secs(1)),
    };
    fixture
        .pane
        .receive_native(
            request(
                fixture
                    .pane
                    .permit(EffectKind::NativeCompletion)
                    .unwrap()
                    .fence(),
            ),
            timing,
        )
        .unwrap();
    fixture.takeover().await;
    fixture.pane.take_over().await.unwrap();
    fixture.frame(1);
    assert_eq!(
        fixture
            .pane
            .receive_native(
                request(
                    fixture
                        .pane
                        .permit(EffectKind::NativeCompletion)
                        .unwrap()
                        .fence()
                ),
                timing
            )
            .unwrap_err()
            .class(),
        ClientFailureClass::StaleAuthority
    );
}

#[tokio::test]
async fn detach_invalidates_external_leases_before_any_deferred_release() {
    let fixture = Fixture::new(true).await;
    fixture.frame(1);
    let frame = fixture.pane.current_frame().unwrap();
    let permit = fixture.pane.permit(EffectKind::ImeCommit).unwrap();
    let before = fixture.server.received_operations().unwrap().len();
    fixture.lease.invalidate().unwrap();
    assert!(!permit.is_current());
    assert!(!frame.is_paintable());
    fixture.peer.reconnect(2, 2).unwrap();
    assert_eq!(
        fixture.pane.state().borrow().readiness,
        PaneReadiness::Detached
    );
    assert!(fixture.lease.pane(fixture.pane.pane()).is_err());
    assert_eq!(fixture.server.received_operations().unwrap().len(), before);
}

#[tokio::test]
async fn authoritative_runtime_failure_clears_frozen_frame_and_pending_reads() {
    let fixture = Fixture::new(true).await;
    let stamp = fixture.frame(1);
    let frame = fixture.pane.current_frame().unwrap();
    let permit = fixture.pane.permit(EffectKind::Read).unwrap();
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
            ContentPageReply::new(&request, page(stamp.base(), 1, false)).unwrap(),
        ))])
        .unwrap();
    let mut stream = fixture
        .pane
        .read(&permit, PaneReadRequest::Page(request))
        .await
        .unwrap();
    fixture
        .channels
        .invalidate(
            fixture.server.server(),
            ConnectionInvalidation::ConnectionLost,
        )
        .unwrap();
    fixture
        .server
        .event(spaceterm_core::reducer::OrganizationEvent::RuntimeFailed {
            session: stamp.base().session,
            class: spaceterm_core::organization::FailureClass::RuntimeUnavailable,
        })
        .unwrap();
    fixture.peer.reconnect(2, 2).unwrap();
    assert!(frame.frame().is_none());
    assert!(fixture.pane.current_frame().is_none());
    assert!(stream.next().await.is_err());
    assert!(matches!(
        fixture.pane.state().borrow().runtime.unwrap().phase,
        SessionPhase::Failed { .. }
    ));
}

#[tokio::test]
async fn native_completion_keeps_the_server_requests_content_fence_at_final_dequeue() {
    let fixture = Fixture::new(true).await;
    fixture.frame(1);
    let permit = fixture.pane.permit(EffectKind::NativeCompletion).unwrap();
    let fence = fixture.pane.permit(EffectKind::Read).unwrap().fence();
    let request = NativeRequest::new(
        fence,
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
    fixture.frame(2);
    assert!(
        fixture.peer.flush_effects().unwrap().is_empty(),
        "a completion must not lose the original content-sensitive request fence"
    );
}

#[tokio::test]
async fn native_clipboard_read_loses_eligibility_when_input_becomes_hidden() {
    let fixture = Fixture::new(true).await;
    fixture.frame(1);
    let permit = fixture.pane.permit(EffectKind::NativeCompletion).unwrap();
    let request = NativeRequest::new(
        permit.fence(),
        NativeRequestId::new(1).unwrap(),
        MonotonicTick::new(0),
        MonotonicTick::new(0),
        Some(MonotonicTick::new(1000)),
        NativeRequestKind::ClipboardRead,
    )
    .unwrap();
    let mut events = fixture.pane.events().unwrap();
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
    let PaneEvent::Native { request: lease, .. } = events.next().await.unwrap().unwrap() else {
        panic!("native event required")
    };
    assert!(lease.is_current());
    let stamp = fixture.frame(2);
    let mut parts = page(stamp.base(), 1, false).into_parts();
    parts.modes.hidden_input = true;
    fixture
        .pane
        .projection()
        .lock()
        .unwrap()
        .accept_page(
            stamp,
            spaceterm_core::content::ContentPage::new(parts).unwrap(),
        )
        .unwrap();
    assert!(!lease.is_current());
    assert!(
        fixture
            .pane
            .complete_native(
                &permit,
                NativeCompletion::new(
                    &request,
                    NativeCompletionKind::ClipboardText(
                        NativeText::new("clipboard".into()).unwrap()
                    )
                )
                .unwrap()
            )
            .is_err()
    );
    assert!(fixture.peer.flush_effects().unwrap().is_empty());
}

fn permission_request(fence: spaceterm_core::authority::EffectFence, id: u64) -> NativeRequest {
    NativeRequest::new(
        fence,
        NativeRequestId::new(id).unwrap(),
        MonotonicTick::new(0),
        MonotonicTick::new(10),
        None,
        NativeRequestKind::Permission(PermissionNotice::new(true, false).unwrap()),
    )
    .unwrap()
}

async fn next_native(events: &mut PaneEvents) -> (NativeRequestLease, NativeTiming) {
    let event = tokio::time::timeout(Duration::from_millis(100), events.next())
        .await
        .expect("pending native request must be resynchronized")
        .unwrap()
        .unwrap();
    let PaneEvent::Native { request, timing } = event else {
        panic!("native event required")
    };
    (request, timing)
}

#[tokio::test(start_paused = true)]
async fn native_subscription_resynchronizes_pending_work_without_extending_deadlines_or_reexecution()
 {
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
    tokio::time::advance(Duration::from_millis(400)).await;
    let mut events = fixture.pane.events().unwrap();
    let (lease, timing) = next_native(&mut events).await;
    assert_eq!(*lease, request);
    assert_eq!(timing.expires_after, Some(Duration::from_millis(600)));
    assert!(lease.is_current());
    fixture
        .pane
        .complete_native(
            &permit,
            NativeCompletion::new(&request, NativeCompletionKind::ClipboardWritten).unwrap(),
        )
        .unwrap();
    drop(events);
    let mut events = fixture.pane.events().unwrap();
    let next = permission_request(permit.fence(), 2);
    fixture
        .pane
        .receive_native(
            next.clone(),
            NativeTiming {
                not_before: Duration::from_millis(10),
                expires_after: None,
            },
        )
        .unwrap();
    let (lease, _) = next_native(&mut events).await;
    assert_eq!(*lease, next, "completed work must not be resubscribed");
}

#[tokio::test(start_paused = true)]
async fn native_subscription_resynchronizes_no_expiry_notice_at_reliable_lane_overflow() {
    let fixture = Fixture::new(true).await;
    let mut old = fixture.pane.events().unwrap();
    fixture.takeover().await;
    fixture.pane.take_over().await.unwrap();
    fixture.frame(1);
    let fence = fixture
        .pane
        .permit(EffectKind::NativeCompletion)
        .unwrap()
        .fence();
    for id in 1..=55 {
        fixture
            .pane
            .receive_native(
                permission_request(fence, id),
                NativeTiming {
                    not_before: Duration::from_millis(10),
                    expires_after: None,
                },
            )
            .unwrap();
    }
    assert_eq!(
        old.next().await.unwrap_err().class(),
        ClientFailureClass::ResourceLimit
    );
    let mut events = fixture.pane.events().unwrap();
    for id in 1..=55 {
        let (lease, timing) = next_native(&mut events).await;
        assert_eq!(lease.id().get(), id);
        assert_eq!(lease.fence(), fence);
        assert_eq!(timing.expires_after, None);
    }
}

#[tokio::test]
async fn reconnect_keeps_retained_content_paint_only_until_fresh_content_base_and_pages() {
    use spaceterm_core::{authority::*, content::*, organization::ControlRecord};
    for changed_output in [false, true] {
        let fixture = Fixture::new(true).await;
        let old_stamp = fixture.frame(1);
        fixture
            .channels
            .invalidate(
                fixture.server.server(),
                ConnectionInvalidation::ConnectionLost,
            )
            .unwrap();
        fixture.peer.reconnect(2, 2).unwrap();
        let retained = fixture.pane.current_frame().unwrap();
        assert!(retained.is_paintable());
        assert!(
            !retained.permits_native_content_use(),
            "organization reconciliation does not confirm content"
        );
        assert!(retained.frozen());
        assert_eq!(
            fixture.pane.state().borrow().readiness,
            PaneReadiness::Reconciling
        );
        assert!(fixture.pane.permit(EffectKind::ImeCommit).is_err());
        assert!(fixture.pane.permit(EffectKind::NativeCompletion).is_err());
        let state = *fixture.pane.state().borrow();
        let info = state.connection.unwrap();
        let runtime = state.runtime.unwrap();
        let ControlRecord::Held { client, generation } = runtime.control else {
            panic!("held control required")
        };
        let fence = EffectFence::new(
            SessionKey {
                pane: fixture.pane.pane(),
                epoch: info.epoch,
                generation: runtime.session_generation,
            },
            ControlGrant::new(fixture.pane.pane(), client, generation).unwrap(),
            info.binding,
            state.context,
            None,
        )
        .unwrap();
        let base = if changed_output {
            ContentBase {
                revision: ContentRevision::new(2),
                ..old_stamp.base()
            }
        } else {
            old_stamp.base()
        };
        fixture.pane.publish_content_base(fence, base).unwrap();
        assert_eq!(fixture.pane.state().borrow().readiness, PaneReadiness::Live);
        if changed_output {
            assert!(
                !fixture
                    .pane
                    .current_frame()
                    .unwrap()
                    .permits_native_content_use()
            );
            let stamp = fixture
                .pane
                .query_stamp(
                    &fixture.pane.permit(EffectKind::Read).unwrap(),
                    RequestGeneration::new(1),
                )
                .unwrap();
            let mut parts = page(base, 1, false).into_parts();
            parts.modes.hidden_input = true;
            fixture
                .pane
                .projection()
                .lock()
                .unwrap()
                .accept_page(stamp, ContentPage::new(parts).unwrap())
                .unwrap();
            assert!(
                fixture
                    .pane
                    .current_frame()
                    .unwrap()
                    .frame()
                    .unwrap()
                    .pages()[0]
                    .page()
                    .parts()
                    .modes
                    .hidden_input
            );
            let request = NativeRequest::new(
                fixture
                    .pane
                    .permit(EffectKind::NativeCompletion)
                    .unwrap()
                    .fence(),
                NativeRequestId::new(1).unwrap(),
                MonotonicTick::new(0),
                MonotonicTick::new(0),
                Some(MonotonicTick::new(1000)),
                NativeRequestKind::ClipboardRead,
            )
            .unwrap();
            assert!(
                fixture
                    .pane
                    .receive_native(
                        request,
                        NativeTiming {
                            not_before: Duration::ZERO,
                            expires_after: Some(Duration::from_secs(1))
                        }
                    )
                    .is_err()
            );
        }
        assert!(
            fixture
                .pane
                .current_frame()
                .unwrap()
                .permits_native_content_use()
        );
        assert!(!fixture.pane.current_frame().unwrap().frozen());
    }
}

#[tokio::test]
async fn explicit_selection_compare_acquires_running_or_starting_unclaimed_pane_without_startup() {
    use spaceterm_core::{
        authority::*,
        control::*,
        operations::Command,
        organization::{ControlRecord, StartupReservation},
        reducer::OrganizationEvent,
    };
    for running in [true, false] {
        let fixture = Fixture::new(running).await;
        if !running {
            let state = *fixture.pane.state().borrow();
            let runtime = state.runtime.unwrap();
            fixture
                .server
                .event(OrganizationEvent::StartupReserved(
                    StartupReservation::new(
                        runtime.pane,
                        runtime.session_generation,
                        runtime.control_generation,
                        runtime.control,
                        SessionKey {
                            generation: runtime.session_generation.checked_successor().unwrap(),
                            ..pane_fence(&fixture.pane).session()
                        },
                    )
                    .unwrap(),
                ))
                .unwrap();
            fixture.peer.publish().unwrap();
        }
        let old_context = advance_context(&fixture.pane);
        let session = pane_fence(&fixture.pane).session();
        fixture
            .peer
            .submit(
                Command::ReleaseControl(ReleaseControlRequest {
                    epoch: session.epoch,
                    grant: pane_fence(&fixture.pane).grant(),
                }),
                Some(fixture.workspace),
            )
            .await
            .unwrap();
        fixture.lease.invalidate().unwrap();
        let lease = fixture
            .channels
            .attach(
                fixture.workspace,
                spaceterm_client::view::AttachmentIncarnation::new(2).unwrap(),
            )
            .unwrap();
        let pane = lease.pane(fixture.pane.pane()).unwrap();
        pane.update_context(PaneContextFacts {
            focused_pane: true,
            terminal_input_focus: true,
            presentable: true,
        })
        .unwrap();
        let before = fixture.server.received_operations().unwrap().len();
        let result = pane
            .explicit_use(ExplicitUseSource::ExplicitSelection)
            .await;
        assert!(
            matches!(result, Ok(ExplicitUseOutcome::ControlReady(_))),
            "selection acquires the existing Session after context changes, not a startup token"
        );
        assert!(pane.state().borrow().context > old_context);
        assert_eq!(
            fixture.server.received_operations().unwrap().len(),
            before + 1
        );
        let runtime = pane.state().borrow().runtime.unwrap();
        assert!(
            matches!(runtime.control, ControlRecord::Held { client, .. } if client == fixture.server.binding(1).unwrap().target().client())
        );
        assert_eq!(runtime.session_generation, session.generation);
        assert_eq!(
            matches!(runtime.phase, SessionPhase::Running { .. }),
            running
        );
    }
}

fn advance_context(pane: &PaneChannel) -> spaceterm_core::authority::ContextGeneration {
    for terminal_input_focus in [false, true, false] {
        pane.update_context(PaneContextFacts {
            focused_pane: true,
            terminal_input_focus,
            presentable: true,
        })
        .unwrap();
    }
    pane.state().borrow().context
}

#[tokio::test]
async fn reattach_starts_with_new_context_without_publishing_retained_detached_handles() {
    use spaceterm_client::view::AttachmentIncarnation;
    use spaceterm_core::{control::ReleaseControlRequest, operations::Command};
    let fixture = Fixture::new(false).await;
    let retained = fixture.pane.clone();
    let old_context = advance_context(&fixture.pane);
    let fence = pane_fence(&fixture.pane);
    fixture
        .peer
        .submit(
            Command::ReleaseControl(ReleaseControlRequest {
                epoch: fence.session().epoch,
                grant: fence.grant(),
            }),
            Some(fixture.workspace),
        )
        .await
        .unwrap();
    fixture.lease.invalidate().unwrap();
    let lease = fixture
        .channels
        .attach(fixture.workspace, AttachmentIncarnation::new(2).unwrap())
        .unwrap();
    let pane = lease.pane(retained.pane()).unwrap();
    pane.update_context(PaneContextFacts {
        focused_pane: true,
        terminal_input_focus: true,
        presentable: true,
    })
    .unwrap();
    let token = pane
        .explicit_use(ExplicitUseSource::ExplicitSelection)
        .await
        .expect("reattachment must acquire and begin startup under a newer context")
        .startup()
        .unwrap();
    assert!(token.intent().fence().context() > old_context);
    let ticket = pane.complete_intent(&token, grid()).await.unwrap();
    assert!(matches!(
        *ticket.status().borrow(),
        spaceterm_client::operations::OperationStatus::Completed {
            outcome: spaceterm_core::operations::Outcome::SessionStarted(_),
            ..
        }
    ));
    assert_eq!(retained.state().borrow().readiness, PaneReadiness::Detached);
    assert!(
        retained
            .update_context(PaneContextFacts::default())
            .is_err()
    );
    assert!(
        retained
            .explicit_use(ExplicitUseSource::StartSession)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn dropped_channel_recreation_keeps_context_high_water_and_rejects_old_intent() {
    let Fixture {
        peer,
        channels: _channels,
        lease,
        pane,
        ..
    } = Fixture::new(false).await;
    let old_context = advance_context(&pane);
    let old = pane
        .explicit_use(ExplicitUseSource::StartSession)
        .await
        .unwrap()
        .startup()
        .unwrap();
    pane.cancel_intent(&old);
    let id = pane.pane();
    drop(pane);
    let pane = lease.pane(id).unwrap();
    pane.update_context(PaneContextFacts {
        focused_pane: true,
        terminal_input_focus: true,
        presentable: true,
    })
    .unwrap();
    assert!(pane.complete_intent(&old, grid()).await.is_err());
    let token = pane
        .explicit_use(ExplicitUseSource::StartSession)
        .await
        .expect("dropping the channel must not reset its connection's context high water")
        .startup()
        .unwrap();
    assert!(token.intent().fence().context() > old_context);
    pane.complete_intent(&token, grid()).await.unwrap();
    assert!(matches!(
        pane.state().borrow().runtime.unwrap().phase,
        SessionPhase::Running { .. }
    ));
    assert!(peer.flush_effects().unwrap().is_empty());
}

#[tokio::test]
async fn competing_explicit_selection_of_running_unclaimed_pane_does_not_take_over_or_start() {
    use spaceterm_client::{projection::ProjectionBudgets, testing::FakePaneConnection};
    use spaceterm_core::{control::ReleaseControlRequest, operations::Command};
    let fixture = Fixture::new(true).await;
    let session = pane_fence(&fixture.pane).session();
    fixture
        .peer
        .submit(
            Command::ReleaseControl(ReleaseControlRequest {
                epoch: session.epoch,
                grant: pane_fence(&fixture.pane).grant(),
            }),
            Some(fixture.workspace),
        )
        .await
        .unwrap();
    let competing = FakePaneConnection::new(fixture.server.clone(), 2, 2)
        .await
        .unwrap();
    let channels = competing
        .channels(ProjectionBudgets::supported().unwrap())
        .unwrap();
    let lease = channels
        .attach(
            fixture.workspace,
            spaceterm_client::view::AttachmentIncarnation::new(1).unwrap(),
        )
        .unwrap();
    let pane = lease.pane(fixture.pane.pane()).unwrap();
    pane.update_context(PaneContextFacts {
        focused_pane: true,
        terminal_input_focus: true,
        presentable: true,
    })
    .unwrap();
    assert!(
        fixture
            .pane
            .explicit_use(ExplicitUseSource::ExplicitSelection)
            .await
            .is_ok()
    );
    assert_eq!(
        pane.explicit_use(ExplicitUseSource::ExplicitSelection)
            .await
            .err()
            .unwrap()
            .class(),
        ClientFailureClass::StaleAuthority
    );
    assert_eq!(pane.state().borrow().readiness, PaneReadiness::Takeover);
    assert_eq!(
        fixture.server.snapshot().unwrap().runtime()[0].session_generation,
        session.generation
    );
}

#[tokio::test]
async fn startup_and_resize_preserve_cells_with_independent_pty_pixel_overflow() {
    use spaceterm_core::{geometry::*, operations::Command};
    for (pixels, expected) in [
        ((64, 16), (0, 16384)),
        ((16, 64), (16384, 0)),
        ((64, 64), (0, 0)),
    ] {
        let fixture = Fixture::new(false).await;
        let grid = MeasuredGrid::new(
            1024,
            1024,
            CellPixels::new(pixels.0, pixels.1).unwrap(),
            SUPPORTED_GRID_LIMITS,
        )
        .unwrap();
        let token = fixture
            .pane
            .explicit_use(ExplicitUseSource::StartSession)
            .await
            .unwrap()
            .startup()
            .unwrap();
        let ticket = fixture.pane.complete_intent(&token, grid).await.unwrap();
        let saved = fixture
            .peer
            .journal()
            .snapshot()
            .unwrap()
            .operation(ticket.id())
            .unwrap()
            .clone();
        let request = spaceterm_core::operation_ledger::CanonicalOperationRequest::decode(
            &saved.request,
            fixture.peer.journal().codec(),
        )
        .unwrap();
        assert!(
            matches!(request.command(), Command::CompleteMeasuredStartup(measurement) if measurement.grid == grid && measurement.grid.pty_pixels() == expected)
        );
        assert!(matches!(
            fixture.pane.state().borrow().runtime.unwrap().phase,
            SessionPhase::Running { .. }
        ));
        fixture.frame(1);
        let permit = fixture.pane.permit(EffectKind::Resize).unwrap();
        fixture
            .pane
            .try_effect(&permit, RuntimeEffect::Resize(grid))
            .unwrap();
        let commands = fixture.peer.flush_effects().unwrap();
        assert!(
            matches!(commands[0].command, RuntimeEffect::Resize(measured) if measured == grid && measured.pty_pixels() == expected)
        );
    }
}

#[tokio::test]
async fn fake_startup_does_not_report_started_when_prior_runtime_cleanup_blocks_reservation() {
    use spaceterm_core::{
        operations::{OperationFailure, Outcome},
        organization::FailureClass,
        reducer::OrganizationEvent,
    };
    let fixture = Fixture::new(true).await;
    let session = pane_fence(&fixture.pane).session();
    fixture
        .server
        .event(OrganizationEvent::RuntimeFailed {
            session,
            class: FailureClass::RuntimeUnavailable,
        })
        .unwrap();
    fixture.peer.publish().unwrap();
    let failed = fixture.pane.state().borrow().runtime.unwrap();
    let token = fixture
        .pane
        .explicit_use(ExplicitUseSource::StartSession)
        .await
        .unwrap()
        .startup()
        .unwrap();
    let ticket = fixture.pane.complete_intent(&token, grid()).await.unwrap();
    assert!(matches!(
        *ticket.status().borrow(),
        spaceterm_client::operations::OperationStatus::Completed {
            outcome: Outcome::Failed(OperationFailure::BusyRetiring),
            ..
        }
    ));
    assert_eq!(
        fixture
            .pane
            .state()
            .borrow()
            .runtime
            .unwrap()
            .session_generation,
        failed.session_generation
    );
}

#[tokio::test]
async fn organization_publication_preserves_the_original_pending_startup_request() {
    let fixture = Fixture::new(false).await;
    let response = fixture.peer.delay_next_intent_response().unwrap();
    let pane = fixture.pane.clone();
    let pending =
        tokio::spawn(async move { pane.explicit_use(ExplicitUseSource::StartSession).await });
    wait_for_operations(&fixture, 2).await;
    let original = *fixture.pane.state().borrow();
    fixture
        .channels
        .invalidate(fixture.server.server(), ConnectionInvalidation::Publication)
        .unwrap();
    assert!(fixture.pane.permit(EffectKind::Read).is_err());
    fixture.peer.publish().unwrap();
    assert_eq!(fixture.pane.state().borrow().context, original.context);
    response.add_permits(1);
    let token = pending
        .await
        .unwrap()
        .expect("an unchanged authenticated publication cannot cancel the user's pending request")
        .startup()
        .unwrap();
    fixture.pane.complete_intent(&token, grid()).await.unwrap();
    assert!(matches!(
        fixture.pane.state().borrow().runtime.unwrap().phase,
        SessionPhase::Running { .. }
    ));
}
