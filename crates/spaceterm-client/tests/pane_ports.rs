#![cfg(feature = "test-support")]
#[path = "projection/support.rs"]
mod support;
use spaceterm_client::{pane_channel::*, projection::*};
use spaceterm_core::{authority::ContentRevision, content::*, content_queries::*, observations::*};
use spaceterm_terminal::{metadata::*, runtime_contracts::*};
use support::*;

fn metadata(pane: &PaneChannel, sequence: u64) -> RuntimeMetadataSnapshot {
    RuntimeMetadataSnapshot::new(RuntimeMetadataParts {
        session: pane_fence(pane).session(),
        sequence: RuntimeObservationSequence::new(sequence).unwrap(),
        observed_at: MonotonicTick::new(sequence),
        title: TitleMetadata {
            value: "title".into(),
            provenance: TitleProvenance::TerminalControl,
        },
        origin: RuntimeMetadataOrigin::Local {
            user: None,
            host: None,
        },
        progress: ProgressMetadata::None,
        freshness: MetadataFreshness::default(),
        prompt_zone: PromptZone::default(),
        hidden_input: false,
    })
    .unwrap()
}

#[tokio::test]
async fn metadata_is_retained_without_a_frame_and_rejects_old_source_sequences() {
    let fixture = Fixture::new(true).await;
    let first = metadata(&fixture.pane, 1);
    let second = metadata(&fixture.pane, 2);
    fixture
        .pane
        .publish_metadata(pane_fence(&fixture.pane), first.clone())
        .unwrap();
    fixture
        .pane
        .publish_metadata(pane_fence(&fixture.pane), second.clone())
        .unwrap();
    assert_eq!(fixture.pane.metadata_snapshot().as_deref(), Some(&second));
    assert!(
        fixture
            .pane
            .publish_metadata(pane_fence(&fixture.pane), first)
            .is_err()
    );
    let mut events = fixture.pane.events().unwrap();
    assert!(
        matches!(events.next().await.unwrap(), Some(PaneEvent::Metadata { snapshot, .. }) if *snapshot == second)
    );
    fixture.takeover().await;
    assert!(fixture.pane.metadata_snapshot().is_none());
}

#[tokio::test]
async fn retained_metadata_reaches_replacement_subscribers_after_focus_changes() {
    let fixture = Fixture::new(true).await;
    let fence = pane_fence(&fixture.pane);
    let snapshot = metadata(&fixture.pane, 1);
    fixture
        .pane
        .publish_metadata(fence, snapshot.clone())
        .unwrap();
    let mut events = fixture.pane.events().unwrap();
    events.next().await.unwrap();
    drop(events);
    fixture
        .pane
        .update_context(PaneContextFacts {
            focused_pane: false,
            terminal_input_focus: false,
            presentable: true,
        })
        .unwrap();
    let current = pane_fence(&fixture.pane);
    assert_ne!(current.context(), fence.context());
    assert!(
        fixture
            .pane
            .publish_metadata(fence, metadata(&fixture.pane, 2))
            .is_err()
    );
    assert!(
        fixture
            .pane
            .publish_metadata(current, snapshot.clone())
            .is_err()
    );
    let mut events = fixture.pane.events().unwrap();
    let event = tokio::time::timeout(std::time::Duration::from_secs(1), events.next())
        .await
        .expect("retained metadata must not wait for a newer source observation")
        .unwrap();
    assert!(
        matches!(event, Some(PaneEvent::Metadata { fence, snapshot: received }) if fence == current && *received == snapshot)
    );
}

#[tokio::test]
async fn retained_metadata_reaches_replacement_subscribers_after_same_session_reconnect() {
    let fixture = Fixture::new(true).await;
    let fence = pane_fence(&fixture.pane);
    let snapshot = metadata(&fixture.pane, 1);
    fixture
        .pane
        .publish_metadata(fence, snapshot.clone())
        .unwrap();
    let mut events = fixture.pane.events().unwrap();
    events.next().await.unwrap();
    drop(events);
    fixture
        .channels
        .invalidate(
            fixture.server.server(),
            spaceterm_client::ports::ConnectionInvalidation::ConnectionLost,
        )
        .unwrap();
    fixture.peer.reconnect(2, 2).unwrap();
    let current = pane_fence(&fixture.pane);
    assert_eq!(current.session(), fence.session());
    assert_ne!(current.connection(), fence.connection());
    assert!(
        fixture
            .pane
            .publish_metadata(fence, metadata(&fixture.pane, 2))
            .is_err()
    );
    assert!(
        fixture
            .pane
            .publish_metadata(current, snapshot.clone())
            .is_err()
    );
    let mut events = fixture.pane.events().unwrap();
    let event = tokio::time::timeout(std::time::Duration::from_secs(1), events.next())
        .await
        .expect("reconciled same-Session metadata must not wait for a newer source observation")
        .unwrap();
    assert!(
        matches!(event, Some(PaneEvent::Metadata { fence, snapshot: received }) if fence == current && *received == snapshot)
    );
}

#[tokio::test]
async fn find_reports_total_matches_without_retaining_all_chunks() {
    let fixture = Fixture::new(true).await;
    let stamp = fixture.frame(1);
    let projection = fixture.pane.projection();
    let mut projection = projection.lock().unwrap();
    let generation = projection.find().generation();
    let stamp = QueryStamp::new(stamp.fence(), stamp.base(), stamp.request(), generation).unwrap();
    let request =
        FindRequest::new(stamp, BoundedFindQuery::new("a".into()).unwrap(), None).unwrap();
    let matched = FindMatch::new(stamp, selection(stamp).start(), selection(stamp).end()).unwrap();
    let find = projection.find_mut().unwrap();
    find.begin(request.clone()).unwrap();
    for index in 0..7 {
        find.accept(FindStreamItem::Chunk(Box::new(
            FindChunk::new(&request, index, vec![matched; MAX_FIND_CHUNK_MATCHES], None).unwrap(),
        )))
        .unwrap();
    }
    assert_eq!(find.total_matches(), (7 * MAX_FIND_CHUNK_MATCHES) as u64);
    assert!(!find.is_complete());
    assert_eq!(
        find.current_window().unwrap().matches.len(),
        MAX_FIND_CHUNK_MATCHES
    );
    find.accept(FindStreamItem::Complete { stamp, chunks: 7 })
        .unwrap();
    assert!(find.is_complete());
    assert_eq!(find.navigate(true), FindNavigation::Match(matched));
    assert_eq!(
        find.current_cursor(),
        Some(FindCursor { chunk: 0, index: 0 })
    );
    find.clear().unwrap();
    assert_eq!(find.total_matches(), 0);
    assert!(find.current_window().is_none());
    assert!(find.current_cursor().is_none());
}

#[tokio::test]
async fn paste_admission_and_write_acknowledgement_keep_the_original_fence() {
    let fixture = Fixture::new(true).await;
    let mut events = fixture.pane.events().unwrap();
    let permit = fixture.pane.permit(EffectKind::Paste).unwrap();
    let admission = fixture
        .pane
        .try_effect(
            &permit,
            RuntimeEffect::RequestPaste(
                spaceterm_core::native_requests::NativeText::new("safe".into()).unwrap(),
            ),
        )
        .unwrap();
    let correlation = admission.correlation().unwrap();
    assert_eq!(correlation.fence(), permit.fence());
    fixture
        .pane
        .receive_runtime_outcome(correlation, Ok(()))
        .unwrap();
    assert!(
        matches!(events.next().await.unwrap(), Some(PaneEvent::PasteWritten { correlation: received, result: Ok(()) }) if received == correlation)
    );
    assert!(
        fixture
            .pane
            .receive_runtime_outcome(correlation, Ok(()))
            .is_err()
    );
    let old = fixture
        .pane
        .try_effect(
            &permit,
            RuntimeEffect::RequestPaste(
                spaceterm_core::native_requests::NativeText::new("delayed".into()).unwrap(),
            ),
        )
        .unwrap()
        .correlation()
        .unwrap();
    fixture.peer.reconnect(2, 2).unwrap();
    assert!(fixture.pane.receive_runtime_outcome(old, Ok(())).is_err());
    assert!(matches!(
        events.next().await.unwrap(),
        Some(PaneEvent::Invalidated)
    ));
}

#[tokio::test]
async fn reconnect_during_effect_admission_does_not_poison_restarted_request_numbers() {
    use spaceterm_client::failure::ClientFailureClass;
    use std::sync::atomic::{AtomicBool, Ordering};
    let fixture = Fixture::new(true).await;
    let done = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let admissions = scope.spawn(|| {
            let mut accepted = 0;
            while !done.load(Ordering::Acquire) {
                if let Ok(permit) = fixture.pane.permit(EffectKind::FocusReport) {
                    match fixture.pane.try_effect(&permit, RuntimeEffect::FocusReport(true)) {
                        Ok(_) => accepted += 1,
                        Err(failure) => assert_ne!(failure.class(), ClientFailureClass::InvalidPeer, "an old admission must not advance the replacement connection's request high-water"),
                    }
                    fixture.peer.flush_effects().unwrap();
                }
                std::thread::yield_now();
            }
            accepted
        });
        let mut failure = None;
        for generation in 2..=10_000 {
            fixture.peer.reconnect(generation, generation).unwrap();
            let fence = pane_fence(&fixture.pane);
            fixture
                .pane
                .publish_content_base(
                    fence,
                    spaceterm_core::content::ContentBase {
                        session: fence.session(),
                        revision: spaceterm_core::authority::ContentRevision::new(0),
                        screen: spaceterm_core::content::Screen::Primary,
                        screen_generation: 1,
                        topology_revision: 1,
                    },
                )
                .unwrap();
            let permit = fixture.pane.permit(EffectKind::FocusReport).unwrap();
            if let Err(error) = fixture
                .pane
                .try_effect(&permit, RuntimeEffect::FocusReport(true))
            {
                failure = Some(error);
                break;
            }
            fixture.peer.flush_effects().unwrap();
            std::thread::yield_now();
        }
        done.store(true, Ordering::Release);
        assert!(admissions.join().unwrap() > 0);
        assert!(
            failure.is_none(),
            "current connection admission failed: {failure:?}"
        );
    });
    let permit = fixture.pane.permit(EffectKind::FocusReport).unwrap();
    fixture
        .pane
        .try_effect(&permit, RuntimeEffect::FocusReport(true))
        .unwrap();
}

#[tokio::test]
async fn enhanced_copy_is_a_distinct_effect_not_an_ordinary_legacy_key() {
    let fixture = Fixture::new(true).await;
    let modifiers = spaceterm_terminal::InputModifiers {
        platform: true,
        ..Default::default()
    };
    let permit = fixture.pane.permit(EffectKind::Key).unwrap();
    fixture
        .pane
        .try_effect(&permit, PaneRuntimeEffect::Copy(modifiers))
        .unwrap();
    assert!(fixture.peer.flush_effects().unwrap().is_empty());
    assert_eq!(
        fixture.peer.flush_copies().unwrap(),
        vec![(permit.fence(), modifiers)]
    );
    fixture.takeover().await;
    assert!(
        fixture
            .pane
            .try_effect(&permit, PaneRuntimeEffect::Copy(modifiers))
            .is_err()
    );
}

#[tokio::test]
async fn paste_approval_and_resolution_preserve_both_request_identities() {
    use spaceterm_core::native_requests::*;
    use std::time::Duration;
    let fixture = Fixture::new(true).await;
    let mut events = fixture.pane.events().unwrap();
    let permit = fixture.pane.permit(EffectKind::Paste).unwrap();
    let text = NativeText::new("unsafe\npaste".into()).unwrap();
    let correlation = fixture
        .pane
        .try_effect(&permit, RuntimeEffect::RequestPaste(text.clone()))
        .unwrap()
        .correlation()
        .unwrap();
    let approval = PasteApprovalId::new(77).unwrap();
    let request = NativeRequest::new(
        correlation.fence(),
        NativeRequestId::new(91).unwrap(),
        MonotonicTick::new(0),
        MonotonicTick::new(0),
        Some(MonotonicTick::new(30_000)),
        NativeRequestKind::UnsafePaste { approval, text },
    )
    .unwrap();
    fixture
        .pane
        .receive_paste_approval(
            correlation,
            request.clone(),
            NativeTiming {
                not_before: Duration::ZERO,
                expires_after: Some(Duration::from_secs(30)),
            },
        )
        .unwrap();
    assert!(
        matches!(events.next().await.unwrap(), Some(PaneEvent::PasteApproval { correlation: received, request: native, .. }) if received == correlation && native.id() == request.id())
    );
    assert!(
        fixture
            .pane
            .receive_runtime_outcome(correlation, Ok(()))
            .is_err()
    );
    let resolved = fixture
        .pane
        .try_effect(
            &permit,
            RuntimeEffect::ResolvePaste(approval, PasteDecision::Confirm),
        )
        .unwrap()
        .correlation()
        .unwrap();
    assert_ne!(resolved.request(), correlation.request());
    let sent = fixture.peer.flush_effects().unwrap();
    assert!(
        matches!(&sent.last().unwrap().command, RuntimeEffect::ResolvePaste(received, PasteDecision::Confirm) if *received == approval)
    );
    assert_eq!(sent.last().unwrap().fence, request.fence());
    fixture
        .pane
        .receive_runtime_outcome(resolved, Ok(()))
        .unwrap();
    assert!(
        matches!(events.next().await.unwrap(), Some(PaneEvent::PasteResolved { correlation: received, approval: received_approval, decision: PasteDecision::Confirm, result: Ok(()) }) if received == resolved && received_approval == approval)
    );
}

#[tokio::test(start_paused = true)]
async fn paste_cancellation_keeps_the_canonical_command_and_original_deadline() {
    use spaceterm_core::native_requests::*;
    let fixture = Fixture::new(true).await;
    let mut events = fixture.pane.events().unwrap();
    let permit = fixture.pane.permit(EffectKind::Paste).unwrap();
    let fence = fixture.pane.permit(EffectKind::Read).unwrap().fence();
    let approval = PasteApprovalId::new(77).unwrap();
    let request = NativeRequest::new(
        fence,
        NativeRequestId::new(91).unwrap(),
        MonotonicTick::new(0),
        MonotonicTick::new(0),
        Some(MonotonicTick::new(30_000)),
        NativeRequestKind::UnsafePaste {
            approval,
            text: NativeText::new("unsafe\npaste".into()).unwrap(),
        },
    )
    .unwrap();
    fixture
        .pane
        .receive_native(
            request.clone(),
            NativeTiming {
                not_before: std::time::Duration::ZERO,
                expires_after: Some(std::time::Duration::from_secs(30)),
            },
        )
        .unwrap();
    events.next().await.unwrap();
    let resolved = fixture
        .pane
        .try_effect(
            &permit,
            RuntimeEffect::ResolvePaste(approval, PasteDecision::Cancel),
        )
        .unwrap()
        .correlation()
        .unwrap();
    let sent = fixture.peer.flush_effects().unwrap();
    assert!(
        matches!(&sent[0].command, RuntimeEffect::ResolvePaste(received, PasteDecision::Cancel) if *received == approval)
    );
    assert_eq!(sent[0].fence, request.fence());
    fixture
        .pane
        .receive_runtime_outcome(
            resolved,
            Err(spaceterm_client::failure::ClientFailure::new(
                spaceterm_client::failure::ClientFailureClass::Cancelled,
            )),
        )
        .unwrap();
    assert!(
        matches!(events.next().await.unwrap(), Some(PaneEvent::PasteResolved { correlation, decision: PasteDecision::Cancel, result: Err(failure), .. }) if correlation == resolved && failure.class() == spaceterm_client::failure::ClientFailureClass::Cancelled)
    );
    assert!(
        fixture
            .pane
            .try_effect(
                &permit,
                RuntimeEffect::ResolvePaste(approval, PasteDecision::Cancel)
            )
            .is_err()
    );

    let approval = PasteApprovalId::new(78).unwrap();
    fixture
        .pane
        .receive_native(
            NativeRequest::new(
                fence,
                NativeRequestId::new(92).unwrap(),
                MonotonicTick::new(0),
                MonotonicTick::new(0),
                Some(MonotonicTick::new(30_000)),
                NativeRequestKind::UnsafePaste {
                    approval,
                    text: NativeText::new("late\npaste".into()).unwrap(),
                },
            )
            .unwrap(),
            NativeTiming {
                not_before: std::time::Duration::ZERO,
                expires_after: Some(std::time::Duration::from_secs(30)),
            },
        )
        .unwrap();
    fixture
        .pane
        .try_effect(
            &permit,
            RuntimeEffect::ResolvePaste(approval, PasteDecision::Cancel),
        )
        .unwrap();
    tokio::time::advance(std::time::Duration::from_secs(30)).await;
    assert!(fixture.peer.flush_effects().unwrap().is_empty());
}

#[tokio::test]
async fn queued_paste_acknowledgement_is_dropped_after_control_loss() {
    use spaceterm_core::native_requests::NativeText;
    let fixture = Fixture::new(true).await;
    let mut events = fixture.pane.events().unwrap();
    let permit = fixture.pane.permit(EffectKind::Paste).unwrap();
    let correlation = fixture
        .pane
        .try_effect(
            &permit,
            RuntimeEffect::RequestPaste(NativeText::new("text".into()).unwrap()),
        )
        .unwrap()
        .correlation()
        .unwrap();
    fixture
        .pane
        .receive_runtime_outcome(correlation, Ok(()))
        .unwrap();
    fixture.takeover().await;
    assert!(matches!(
        events.next().await.unwrap(),
        Some(PaneEvent::Invalidated)
    ));
}

#[tokio::test]
async fn replacement_session_discards_metadata_and_accepts_a_new_source_sequence() {
    use spaceterm_core::{organization::FailureClass, reducer::OrganizationEvent};
    let fixture = Fixture::new(true).await;
    let old = metadata(&fixture.pane, 100);
    let fence = pane_fence(&fixture.pane);
    fixture.pane.publish_metadata(fence, old.clone()).unwrap();
    fixture
        .server
        .event(OrganizationEvent::RuntimeFailed {
            session: fence.session(),
            class: FailureClass::RuntimeFault,
        })
        .unwrap();
    fixture.peer.publish().unwrap();
    assert!(fixture.pane.metadata_snapshot().is_none());
    // Publish the replacement provider's checked snapshot. The test connection
    // does not supply process cleanup, so it cannot launch after RuntimeFailed.
    let old_snapshot = fixture.server.snapshot().unwrap();
    let mut runtime = old_snapshot.runtime().to_vec();
    runtime[0].session_generation = runtime[0].session_generation.checked_successor().unwrap();
    runtime[0].phase = spaceterm_core::organization::SessionPhase::Running {
        generation: runtime[0].session_generation,
    };
    let mut state = old_snapshot.state().clone();
    state.revision = state.revision.checked_successor().unwrap();
    let mut retained = state.retained.as_ref().unwrap().parts().clone();
    retained.pane_generations[0] =
        spaceterm_core::organization_retained::PaneGenerationHighWater::new(
            runtime[0].pane,
            runtime[0].session_generation,
            runtime[0].control_generation,
        );
    state.retained = Some(
        spaceterm_core::organization_retained::OrganizationRetainedFacts::new(retained).unwrap(),
    );
    let snapshot = spaceterm_core::organization::OrganizationSnapshot::new(
        old_snapshot.epoch(),
        state,
        runtime,
    )
    .unwrap();
    let mut replica = spaceterm_client::replica::Replica::new(
        fixture.server.server(),
        std::sync::Arc::new(
            spaceterm_client::replica::OrganizationResidency::new(
                spaceterm_client::limits::ClientLimits::default(),
            )
            .unwrap(),
        ),
    );
    let token = replica.begin_resync(old_snapshot.epoch()).unwrap();
    replica.install_snapshot(token, old_snapshot).unwrap();
    let token = replica.begin_resync(snapshot.epoch()).unwrap();
    replica.install_snapshot(token, snapshot).unwrap();
    let published = replica.subscribe().borrow().retained().unwrap().clone();
    let mut info = fixture.pane.state().borrow().connection.unwrap();
    info.organization_revision = published.cursor.revision;
    fixture.channels.publish(info, published).unwrap();
    let new_fence = pane_fence(&fixture.pane);
    assert_ne!(new_fence.session(), fence.session());
    assert!(fixture.pane.publish_metadata(fence, old).is_err());
    let new = metadata(&fixture.pane, 1);
    fixture
        .pane
        .publish_metadata(new_fence, new.clone())
        .unwrap();
    assert_eq!(fixture.pane.metadata_snapshot().as_deref(), Some(&new));
}

#[tokio::test]
async fn both_wire_majors_preserve_outcome_correlation_and_typed_rejection() {
    use spaceterm_core::native_requests::NativeText;
    use spaceterm_protocol::{
        convert::identity::encode_fence,
        wire::{common, v1, v2},
    };
    for major in [1, 2] {
        let fixture = Fixture::new(true).await;
        let mut events = fixture.pane.events().unwrap();
        let permit = fixture.pane.permit(EffectKind::Paste).unwrap();
        let correlation = fixture
            .pane
            .try_effect(
                &permit,
                RuntimeEffect::RequestPaste(NativeText::new("text".into()).unwrap()),
            )
            .unwrap()
            .correlation()
            .unwrap();
        let failure = Some(common::Failure {
            classification: common::FailureClass::Denied as i32,
            retry_after_ms: 0,
        });
        if major == 1 {
            let mut outcome = v1::RuntimeOutcome {
                request: correlation.request(),
                fence: Some(encode_fence(correlation.fence())),
                failure,
            };
            let original = outcome.fence.clone();
            outcome.fence.as_mut().unwrap().connection_generation += 1;
            assert!(fixture.pane.receive_v1_runtime_outcome(&outcome).is_err());
            outcome.fence = original;
            fixture.pane.receive_v1_runtime_outcome(&outcome).unwrap();
        } else {
            let mut outcome = v2::RuntimeOutcome {
                request: correlation.request(),
                fence: Some(encode_fence(correlation.fence())),
                failure,
            };
            let original = outcome.fence.clone();
            outcome.fence.as_mut().unwrap().context_generation += 1;
            assert!(fixture.pane.receive_v2_runtime_outcome(&outcome).is_err());
            outcome.fence = original;
            fixture.pane.receive_v2_runtime_outcome(&outcome).unwrap();
        }
        assert!(
            matches!(events.next().await.unwrap(), Some(PaneEvent::PasteWritten { correlation: received, result: Err(failure) }) if received == correlation && failure.class() == spaceterm_client::failure::ClientFailureClass::AuthorizationDenied)
        );
    }
}

#[tokio::test(start_paused = true)]
async fn paste_completion_expires_and_pending_correlations_are_bounded_before_enqueue() {
    use spaceterm_core::native_requests::NativeText;
    let fixture = Fixture::new(true).await;
    let permit = fixture.pane.permit(EffectKind::Paste).unwrap();
    let mut first = None;
    for _ in 0..spaceterm_protocol::limits::MAX_EGRESS_ENVELOPES {
        let correlation = fixture
            .pane
            .try_effect(
                &permit,
                RuntimeEffect::RequestPaste(NativeText::new("text".into()).unwrap()),
            )
            .unwrap()
            .correlation()
            .unwrap();
        first.get_or_insert(correlation);
        fixture.peer.flush_effects().unwrap();
    }
    assert!(
        fixture
            .pane
            .try_effect(
                &permit,
                RuntimeEffect::RequestPaste(NativeText::new("over limit".into()).unwrap())
            )
            .is_err()
    );
    assert!(fixture.peer.flush_effects().unwrap().is_empty());
    tokio::time::advance(std::time::Duration::from_secs(30)).await;
    assert!(
        fixture
            .pane
            .receive_runtime_outcome(first.unwrap(), Ok(()))
            .is_err()
    );
    assert!(
        fixture
            .pane
            .try_effect(
                &permit,
                RuntimeEffect::RequestPaste(NativeText::new("new".into()).unwrap())
            )
            .unwrap()
            .correlation()
            .is_some()
    );
}

#[tokio::test]
async fn metadata_updates_coalesce_without_spending_native_lifecycle_capacity() {
    let fixture = Fixture::new(true).await;
    let mut events = fixture.pane.events().unwrap();
    let mut latest = metadata(&fixture.pane, 1);
    for sequence in 1..=100 {
        latest = metadata(&fixture.pane, sequence);
        fixture
            .pane
            .publish_metadata(pane_fence(&fixture.pane), latest.clone())
            .unwrap();
    }
    assert!(
        matches!(events.next().await.unwrap(), Some(PaneEvent::Metadata { snapshot, .. }) if *snapshot == latest)
    );
}

#[tokio::test]
async fn routed_metadata_requires_existing_live_channel_and_current_fence() {
    let fixture = Fixture::new(true).await;
    let captured = pane_fence(&fixture.pane);
    let first = metadata(&fixture.pane, 1);
    assert!(
        fixture
            .channels
            .receive_metadata(captured, first.clone())
            .unwrap()
    );
    assert_eq!(fixture.pane.metadata_snapshot().as_deref(), Some(&first));
    assert!(!fixture.channels.receive_metadata(captured, first).unwrap());
    let delayed = metadata(&fixture.pane, 2);
    fixture.takeover().await;
    assert!(
        !fixture
            .channels
            .receive_metadata(captured, delayed)
            .unwrap()
    );
    assert!(fixture.pane.metadata_snapshot().is_none());
}

#[tokio::test]
async fn correlated_paste_accepts_native_reordering_but_never_a_different_origin_or_duplicate() {
    use spaceterm_core::native_requests::*;
    use std::time::Duration;
    let fixture = Fixture::new(true).await;
    let mut events = fixture.pane.events().unwrap();
    let permit = fixture.pane.permit(EffectKind::Paste).unwrap();
    let origin = fixture
        .pane
        .try_effect(
            &permit,
            RuntimeEffect::RequestPaste(NativeText::new("one\ntwo".into()).unwrap()),
        )
        .unwrap()
        .correlation()
        .unwrap();
    let notification = NativeRequest::new(
        origin.fence(),
        NativeRequestId::new(80).unwrap(),
        MonotonicTick::new(0),
        MonotonicTick::new(0),
        Some(MonotonicTick::new(1000)),
        NativeRequestKind::Notification(NotificationNotice::Bell),
    )
    .unwrap();
    fixture
        .pane
        .receive_native(
            notification,
            NativeTiming {
                not_before: Duration::ZERO,
                expires_after: Some(Duration::from_secs(1)),
            },
        )
        .unwrap();
    assert!(matches!(
        events.next().await.unwrap(),
        Some(PaneEvent::Native { .. })
    ));
    let approval = PasteApprovalId::new(77).unwrap();
    let request = NativeRequest::new(
        origin.fence(),
        NativeRequestId::new(70).unwrap(),
        MonotonicTick::new(0),
        MonotonicTick::new(0),
        Some(MonotonicTick::new(30000)),
        NativeRequestKind::UnsafePaste {
            approval,
            text: NativeText::new("one\ntwo".into()).unwrap(),
        },
    )
    .unwrap()
    .with_runtime_request(origin.request())
    .unwrap();
    let timing = NativeTiming {
        not_before: Duration::ZERO,
        expires_after: Some(Duration::from_secs(30)),
    };
    let wrong = request
        .clone()
        .with_runtime_request(origin.request() + 1)
        .unwrap();
    assert!(
        fixture
            .pane
            .receive_paste_approval(origin, wrong, timing)
            .is_err()
    );
    fixture
        .pane
        .receive_paste_approval(origin, request.clone(), timing)
        .unwrap();
    let Some(PaneEvent::PasteApproval {
        correlation,
        request: lease,
        ..
    }) = events.next().await.unwrap()
    else {
        panic!("paste approval");
    };
    assert_eq!(correlation, origin);
    assert_eq!(lease.id(), request.id());
    assert_eq!(lease.runtime_request().unwrap().get(), origin.request());
    assert!(
        fixture
            .pane
            .receive_paste_approval(origin, request.clone(), timing)
            .is_err()
    );
    assert!(
        fixture
            .pane
            .receive_runtime_outcome(origin, Ok(()))
            .is_err()
    );
    let forged = request
        .clone()
        .with_runtime_request(origin.request() + 1)
        .unwrap();
    let forged = NativeCompletion::new(
        &forged,
        NativeCompletionKind::Paste {
            approval,
            decision: PasteDecision::Confirm,
        },
    )
    .unwrap();
    let native = fixture.pane.permit(EffectKind::NativeCompletion).unwrap();
    assert!(fixture.pane.complete_native(&native, forged).is_err());
    assert!(lease.is_current());
    let completion = NativeCompletion::new(
        &request,
        NativeCompletionKind::Paste {
            approval,
            decision: PasteDecision::Confirm,
        },
    )
    .unwrap();
    let resolved = fixture
        .pane
        .complete_native(&native, completion.clone())
        .unwrap()
        .correlation()
        .unwrap();
    assert_ne!(resolved.request(), request.id().get());
    assert_ne!(resolved.request(), origin.request());
    assert_eq!(resolved.fence(), origin.fence());
    assert!(fixture.pane.complete_native(&native, completion).is_err());
    fixture
        .pane
        .receive_runtime_outcome(resolved, Ok(()))
        .unwrap();
    assert!(
        matches!(events.next().await.unwrap(), Some(PaneEvent::PasteResolved { correlation, approval: id, result: Ok(()), .. }) if correlation == resolved && id == approval)
    );
}

#[tokio::test(start_paused = true)]
async fn correlated_paste_native_delivery_never_extends_the_originating_runtime_deadline() {
    use spaceterm_core::native_requests::*;
    use std::time::Duration;
    let fixture = Fixture::new(true).await;
    let mut events = fixture.pane.events().unwrap();
    let permit = fixture.pane.permit(EffectKind::Paste).unwrap();
    let origin = fixture
        .pane
        .try_effect(
            &permit,
            RuntimeEffect::RequestPaste(NativeText::new("one\ntwo".into()).unwrap()),
        )
        .unwrap()
        .correlation()
        .unwrap();
    tokio::time::advance(Duration::from_secs(20)).await;
    let approval = PasteApprovalId::new(77).unwrap();
    let request = NativeRequest::new(
        origin.fence(),
        NativeRequestId::new(70).unwrap(),
        MonotonicTick::new(0),
        MonotonicTick::new(0),
        Some(MonotonicTick::new(30000)),
        NativeRequestKind::UnsafePaste {
            approval,
            text: NativeText::new("one\ntwo".into()).unwrap(),
        },
    )
    .unwrap()
    .with_runtime_request(origin.request())
    .unwrap();
    fixture
        .pane
        .receive_paste_approval(
            origin,
            request.clone(),
            NativeTiming {
                not_before: Duration::ZERO,
                expires_after: Some(Duration::from_secs(30)),
            },
        )
        .unwrap();
    let Some(PaneEvent::PasteApproval { request: lease, .. }) = events.next().await.unwrap() else {
        panic!("paste approval");
    };
    assert!(lease.is_current());
    tokio::time::advance(Duration::from_secs(10)).await;
    assert!(!lease.is_current());
    let native = fixture.pane.permit(EffectKind::NativeCompletion).unwrap();
    let completion = NativeCompletion::new(
        &request,
        NativeCompletionKind::Paste {
            approval,
            decision: PasteDecision::Confirm,
        },
    )
    .unwrap();
    assert!(fixture.pane.complete_native(&native, completion).is_err());
    assert!(
        fixture
            .peer
            .flush_effects()
            .unwrap()
            .iter()
            .all(|effect| !matches!(effect.command, RuntimeEffect::CompleteNativeRequest(_)))
    );
}

#[tokio::test]
async fn cold_retained_channels_are_readonly_until_authenticated_publication_and_content() {
    use spaceterm_client::{ports::ConnectionInvalidation, view::AttachmentIncarnation};
    use std::sync::Arc;
    let fixture = Fixture::new(true).await;
    let channels = PaneChannels::new(
        fixture.peer.clone(),
        ProjectionBudgets::supported().unwrap(),
    )
    .unwrap();
    fixture
        .peer
        .journal()
        .bind_pane_channels(channels.clone())
        .unwrap();
    let binding = fixture.server.binding(1).unwrap();
    let snapshot = fixture.server.replica().unwrap();
    let target = binding.target();
    let different_incarnation = spaceterm_client::api::PairedServerBinding::new(
        spaceterm_client::api::ConnectionTarget::new(
            target.pin(),
            target.device(),
            target.pairing(),
            target.client(),
            target.route().clone(),
            target.credential(),
            spaceterm_core::pairing::LocalPairingGeneration::new(target.incarnation().get() + 1)
                .unwrap(),
        )
        .unwrap(),
    );
    assert!(matches!(
        channels.retain(&different_incarnation, Arc::clone(&snapshot)),
        Err(error) if error.class() == spaceterm_client::failure::ClientFailureClass::StaleAuthority
    ));
    channels.retain(&binding, Arc::clone(&snapshot)).unwrap();
    let lease = channels
        .attach(fixture.workspace, AttachmentIncarnation::new(2).unwrap())
        .unwrap();
    let pane = lease.pane(fixture.pane.pane()).unwrap();
    assert_eq!(pane.state().borrow().readiness, PaneReadiness::Offline);
    assert!(pane.state().borrow().connection.is_none());
    assert!(pane.current_frame().is_none());
    pane.update_context(PaneContextFacts {
        focused_pane: true,
        terminal_input_focus: true,
        presentable: true,
    })
    .unwrap();
    for kind in [
        EffectKind::Key,
        EffectKind::Read,
        EffectKind::NativeRead,
        EffectKind::Resize,
    ] {
        assert!(pane.permit(kind).is_err());
    }
    assert!(
        pane.explicit_use(ExplicitUseSource::StartSession)
            .await
            .is_err()
    );
    assert!(pane.take_over().await.is_err());
    let info = fixture.server.connected_info(1, 1, 1, 2).unwrap();
    channels.publish(info, snapshot).unwrap();
    assert_eq!(pane.state().borrow().connection, Some(info));
    assert_eq!(pane.state().borrow().readiness, PaneReadiness::Reconciling);
    assert!(pane.permit(EffectKind::Key).is_err());
    let fence = pane_fence(&pane);
    pane.publish_content_base(
        fence,
        ContentBase {
            session: fence.session(),
            revision: ContentRevision::new(1),
            screen: Screen::Primary,
            screen_generation: 1,
            topology_revision: 1,
        },
    )
    .unwrap();
    assert!(pane.permit(EffectKind::Key).is_ok());
    channels
        .invalidate(
            fixture.server.server(),
            ConnectionInvalidation::BindingRemoved,
        )
        .unwrap();
    assert!(pane.permit(EffectKind::Key).is_err());
    let replacement = channels
        .attach(fixture.workspace, AttachmentIncarnation::new(3).unwrap())
        .unwrap();
    assert!(replacement.pane(fixture.pane.pane()).is_err());
}
