#![cfg(feature = "runtime")]
mod support;
use spaceterm_core::authority::*;
use spaceterm_core::organization::{FailureClass, RetirementReason};
use spaceterm_terminal::runtime::budget::RuntimeLimits;
use spaceterm_terminal::runtime_contracts::*;
use support::*;

#[test]
fn takeover_discards_unwritten_suffix_after_would_block() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    pty.update(|state| state.write_limit = Some(3));
    let mut owner = running(&factory, request.clone(), &pty);
    let old = owner
        .handle()
        .try_submit(fence(request.authority()), ime("old-suffix"))
        .unwrap();
    pty.until(|state| state.written.len() == 3);
    let transition = takeover(request.authority());
    owner
        .transition_authority(transition)
        .unwrap()
        .receive_blocking()
        .unwrap();
    assert_eq!(
        old.receive_blocking().unwrap(),
        EffectDisposition::StaleAuthority
    );
    pty.update(|state| {
        state.blocked = false;
        state.write_limit = None;
    });
    owner
        .handle()
        .try_submit(fence(transition.next()), ime("new"))
        .unwrap()
        .receive_blocking()
        .unwrap();
    assert_eq!(pty.inspect(|state| state.written.clone()), b"oldnew");
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}
#[test]
fn disconnect_discards_input_without_replay_and_preserves_grant() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    pty.update(|state| state.blocked = true);
    let mut owner = running(&factory, request.clone(), &pty);
    let old = owner
        .handle()
        .try_submit(fence(request.authority()), ime("discard"))
        .unwrap();
    let mut disconnected = request.authority().parts();
    disconnected.connection = None;
    disconnected.focused_pane = false;
    disconnected.terminal_input_focus = false;
    disconnected.native_effects_eligible = false;
    let disconnected = RuntimeAuthority::new(disconnected).unwrap();
    owner
        .transition_authority(
            RuntimeAuthorityTransition::new(request.authority(), disconnected).unwrap(),
        )
        .unwrap()
        .receive_blocking()
        .unwrap();
    assert_eq!(
        old.receive_blocking().unwrap(),
        EffectDisposition::StaleAuthority
    );
    assert_eq!(
        disconnected.parts().grant,
        request.authority().parts().grant
    );
    let mut reconnected = request.authority().parts();
    reconnected.connection.as_mut().unwrap().generation = ConnectionGeneration::new(2);
    reconnected.context = ContextGeneration::new(2);
    let reconnected = RuntimeAuthority::new(reconnected).unwrap();
    owner
        .transition_authority(RuntimeAuthorityTransition::new(disconnected, reconnected).unwrap())
        .unwrap()
        .receive_blocking()
        .unwrap();
    assert!(
        owner
            .handle()
            .try_submit(fence(request.authority()), ime("old"))
            .is_err()
    );
    pty.update(|state| state.blocked = false);
    owner
        .handle()
        .try_submit(fence(reconnected), ime("fresh"))
        .unwrap()
        .receive_blocking()
        .unwrap();
    assert_eq!(pty.inspect(|state| state.written.clone()), b"fresh");
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}
#[test]
fn command_count_bound_rejects_limit_plus_one_and_retirement_still_progresses() {
    let limits = RuntimeLimits {
        commands: 2,
        ..RuntimeLimits::default()
    };
    let factory = factory(limits);
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    pty.update(|state| state.blocked = true);
    let mut owner = running(&factory, request.clone(), &pty);
    let handle = owner.handle();
    let first = handle
        .try_submit(fence(request.authority()), ime("one"))
        .unwrap();
    let second = handle
        .try_submit(fence(request.authority()), ime("two"))
        .unwrap();
    assert_eq!(
        handle
            .try_submit(fence(request.authority()), ime("three"))
            .err()
            .unwrap()
            .class,
        FailureClass::ResourceLimit
    );
    let transition = takeover(request.authority());
    owner
        .transition_authority(transition)
        .unwrap()
        .receive_blocking()
        .unwrap();
    assert_eq!(
        first.receive_blocking().unwrap(),
        EffectDisposition::StaleAuthority
    );
    assert_eq!(
        second.receive_blocking().unwrap(),
        EffectDisposition::StaleAuthority
    );
    let third = handle
        .try_submit(fence(transition.next()), ime("three"))
        .unwrap();
    owner.retire(RetirementReason::Stop);
    assert_eq!(
        third.receive_blocking().unwrap(),
        EffectDisposition::Retired
    );
    owner.join().unwrap();
    assert!(pty.inspect(|state| state.written.is_empty()));
}
#[test]
fn dropping_a_reply_cancels_its_unwritten_input_not_the_runtime() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    pty.update(|state| state.blocked = true);
    let mut owner = running(&factory, request.clone(), &pty);
    drop(
        owner
            .handle()
            .try_submit(fence(request.authority()), ime("cancelled"))
            .unwrap(),
    );
    pty.update(|state| state.blocked = false);
    owner
        .handle()
        .try_submit(fence(request.authority()), ime("kept"))
        .unwrap()
        .receive_blocking()
        .unwrap();
    assert_eq!(pty.inspect(|state| state.written.clone()), b"kept");
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}
#[test]
fn ime_requires_focused_pane_but_not_terminal_input_focus() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    let mut parts = request.authority().parts();
    parts.terminal_input_focus = false;
    parts.native_effects_eligible = false;
    let ime_only = RuntimeAuthority::new(parts).unwrap();
    owner
        .transition_authority(
            RuntimeAuthorityTransition::new(request.authority(), ime_only).unwrap(),
        )
        .unwrap()
        .receive_blocking()
        .unwrap();
    assert!(
        owner
            .handle()
            .try_submit(
                fence(ime_only),
                RuntimeCommand::Key(spaceterm_terminal::KeyInput::text_input("key"))
            )
            .is_err()
    );
    assert_eq!(
        owner
            .handle()
            .try_submit(fence(ime_only), ime("IME"))
            .unwrap()
            .receive_blocking()
            .unwrap(),
        EffectDisposition::Applied
    );
    parts.focused_pane = false;
    let unfocused = RuntimeAuthority::new(parts).unwrap();
    owner
        .transition_authority(RuntimeAuthorityTransition::new(ime_only, unfocused).unwrap())
        .unwrap()
        .receive_blocking()
        .unwrap();
    assert!(
        owner
            .handle()
            .try_submit(fence(unfocused), ime("stale"))
            .is_err()
    );
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}
#[test]
fn every_authority_field_is_checked_at_admission() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    let original = fence(request.authority());
    let variants = [
        EffectFence::new(
            SessionKey {
                epoch: ServerEpoch::new(2),
                ..original.session()
            },
            original.grant(),
            original.connection(),
            original.context(),
            None,
        )
        .unwrap(),
        EffectFence::new(
            SessionKey {
                generation: SessionGeneration::new(3),
                ..original.session()
            },
            original.grant(),
            original.connection(),
            original.context(),
            None,
        )
        .unwrap(),
        EffectFence::new(
            original.session(),
            ControlGrant::new(
                original.session().pane,
                original.grant().client(),
                ControlGeneration::new(2),
            )
            .unwrap(),
            original.connection(),
            original.context(),
            None,
        )
        .unwrap(),
        EffectFence::new(
            original.session(),
            original.grant(),
            ConnectionBinding::new(original.connection().client, ConnectionGeneration::new(2)),
            original.context(),
            None,
        )
        .unwrap(),
        EffectFence::new(
            original.session(),
            original.grant(),
            original.connection(),
            ContextGeneration::new(2),
            None,
        )
        .unwrap(),
    ];
    for stale in variants {
        for command in [
            ime("stale"),
            RuntimeCommand::Key(spaceterm_terminal::KeyInput::text_input("stale")),
            RuntimeCommand::Resize(grid(25, 80)),
            RuntimeCommand::Appearance(Box::new(request.appearance().clone())),
            RuntimeCommand::FocusReport(true),
            RuntimeCommand::CellPixelMetrics(
                spaceterm_core::geometry::CellPixels::new(8, 16).unwrap(),
            ),
            RuntimeCommand::ClearScreenAndScrollback,
        ] {
            assert_eq!(
                owner
                    .handle()
                    .try_submit(stale, command)
                    .err()
                    .unwrap()
                    .class,
                FailureClass::StaleAuthority
            );
        }
    }
    assert!(pty.inspect(|state| state.written.is_empty() && state.sizes.len() == 1));
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}
#[test]
fn terminal_replies_do_not_require_client_focus() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    let mut parts = request.authority().parts();
    parts.connection = None;
    parts.focused_pane = false;
    parts.terminal_input_focus = false;
    parts.native_effects_eligible = false;
    owner
        .transition_authority(
            RuntimeAuthorityTransition::new(
                request.authority(),
                RuntimeAuthority::new(parts).unwrap(),
            )
            .unwrap(),
        )
        .unwrap()
        .receive_blocking()
        .unwrap();
    pty.output(b"query");
    pty.until(|state| state.written == b"reply");
    assert_eq!(owner.lifecycle_snapshot(), RuntimeLifecycle::Running);
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}
#[test]
fn queued_content_result_is_checked_again_on_receive() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    let reply = owner
        .handle()
        .read_topology(topology(stamp(request.authority(), 1, 1, 1)))
        .unwrap();
    pty.until(|state| state.content_reads == 1);
    owner
        .transition_authority(takeover(request.authority()))
        .unwrap()
        .receive_blocking()
        .unwrap();
    assert_eq!(
        reply.receive_blocking().unwrap_err().class,
        FailureClass::StaleAuthority
    );
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}
#[test]
fn newer_query_generation_invalidates_an_unread_old_result() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    let old = owner
        .handle()
        .read_topology(topology(stamp(request.authority(), 1, 1, 1)))
        .unwrap();
    pty.until(|state| state.content_reads == 1);
    let new = owner
        .handle()
        .read_topology(topology(stamp(request.authority(), 1, 1, 2)))
        .unwrap();
    assert_eq!(
        old.receive_blocking().unwrap_err().class,
        FailureClass::StaleAuthority
    );
    assert!(matches!(
        new.receive_blocking().unwrap(),
        spaceterm_core::content_queries::ContentReadResult::Ready(_)
    ));
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}
#[test]
fn slow_resource_consumer_does_not_stall_input_and_takeover_revokes_buffered_chunks() {
    use spaceterm_core::content_queries::*;
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    let query = FindRequest::new(
        stamp(request.authority(), 1, 1, 1),
        BoundedFindQuery::new("query".into()).unwrap(),
        None,
    )
    .unwrap();
    let mut stream = owner.handle().find(query).unwrap();
    pty.until(|state| state.content_steps == 2);
    owner
        .handle()
        .try_submit(fence(request.authority()), ime("input"))
        .unwrap()
        .receive_blocking()
        .unwrap();
    assert_eq!(pty.inspect(|state| state.content_steps), 2);
    owner
        .transition_authority(takeover(request.authority()))
        .unwrap()
        .receive_blocking()
        .unwrap();
    assert_eq!(
        stream.next_blocking().unwrap_err().class,
        FailureClass::StaleAuthority
    );
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}

#[test]
fn stale_appearance_generation_is_rejected_before_engine_effect() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    let mut appearance = request.appearance().clone();
    appearance.generation = spaceterm_terminal::appearance::AppearanceGeneration::new(0);
    let result = owner
        .handle()
        .try_submit(
            fence(request.authority()),
            RuntimeCommand::Appearance(Box::new(appearance)),
        )
        .unwrap()
        .receive_blocking()
        .unwrap();
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
    assert_eq!(result, EffectDisposition::StaleAuthority);
    assert!(pty.inspect(|state| state.appearance_generations.is_empty()));
}

#[test]
fn aggregate_pending_input_preserves_each_owners_terminal_reply_reserve() {
    let limits = RuntimeLimits {
        owners: 2,
        aggregate_pending_input_bytes: 2 * 65_536 + 256,
        ..RuntimeLimits::default()
    };
    let factory = factory(limits);
    let first_request = request(1, grid(24, 80));
    let second_request = request(2, grid(24, 80));
    let first_pty = Pty::default();
    let second_pty = Pty::default();
    first_pty.update(|state| {
        state.blocked = true;
        state.expand_input = true;
    });
    second_pty.update(|state| state.blocked = true);
    let mut first = running(&factory, first_request.clone(), &first_pty);
    let mut second = running(&factory, second_request.clone(), &second_pty);
    let input = first
        .handle()
        .try_submit(fence(first_request.authority()), ime("0123456789abcdef"))
        .unwrap();
    first_pty.until(|state| state.input_commands == 1);
    let rejected = second
        .handle()
        .try_submit(fence(second_request.authority()), ime("new"))
        .err();
    first_pty.update(|state| state.blocked = false);
    second_pty.update(|state| state.blocked = false);
    input.receive_blocking().unwrap();
    first_pty.output(b"query");
    second_pty.output(b"query");
    first_pty.until(|state| state.written.ends_with(b"reply"));
    second_pty.until(|state| state.written == b"reply");
    first.retire(RetirementReason::Stop);
    second.retire(RetirementReason::Stop);
    first.join().unwrap();
    second.join().unwrap();
    assert_eq!(
        rejected.map(|error| error.class),
        Some(FailureClass::ResourceLimit)
    );
}

#[test]
fn topology_revision_change_returns_correlated_resync_not_stale_authority() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    let old = stamp(request.authority(), 1, 7, 3);
    pty.output(b"new output");
    pty.until(|state| state.engine_bytes == b"new output");
    owner
        .handle()
        .try_submit(fence(request.authority()), ime("barrier"))
        .unwrap()
        .receive_blocking()
        .unwrap();
    let reply = owner.handle().read_topology(topology(old));
    let result = reply.and_then(|reply| reply.receive_blocking());
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
    match result.unwrap() {
        spaceterm_core::content_queries::ContentReadResult::ResyncRequired { request, current } => {
            assert_eq!(request, old);
            assert_eq!(current.session, old.base().session);
            assert_eq!(current.revision, ContentRevision::new(2));
        }
        _ => panic!("expected resync"),
    }
}

#[test]
fn command_byte_bound_rejects_limit_plus_one_before_effect() {
    let limits = RuntimeLimits {
        command_bytes: std::mem::size_of::<RuntimeCommand>() + 3,
        ..RuntimeLimits::default()
    };
    let factory = factory(limits);
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    assert_eq!(
        owner
            .handle()
            .try_submit(fence(request.authority()), ime("four"))
            .err()
            .unwrap()
            .class,
        FailureClass::ResourceLimit
    );
    owner
        .handle()
        .try_submit(fence(request.authority()), ime("abc"))
        .unwrap()
        .receive_blocking()
        .unwrap();
    assert_eq!(pty.inspect(|state| state.written.clone()), b"abc");
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}
#[test]
fn aggregate_ingress_reservations_bound_multiple_starting_runtimes() {
    use std::sync::{Arc, Barrier};
    let limits = RuntimeLimits {
        aggregate_command_bytes: 2 * (std::mem::size_of::<RuntimeCommand>() + 1) - 1,
        ..RuntimeLimits::default()
    };
    let factory = factory(limits);
    let first_request = request(1, grid(24, 80));
    let second_request = request(2, grid(24, 80));
    let first_pty = Pty::default();
    let second_pty = Pty::default();
    let entered = Arc::new(Barrier::new(3));
    let release = Arc::new(Barrier::new(3));
    let first_launch = first_pty.launch();
    let start = Arc::clone(&entered);
    let finish = Arc::clone(&release);
    let mut first = factory
        .start(
            first_request.clone(),
            Box::new(move |size| {
                start.wait();
                finish.wait();
                first_launch(size)
            }),
            engine_for(first_pty.clone()),
        )
        .unwrap();
    let second_launch = second_pty.launch();
    let start = Arc::clone(&entered);
    let finish = Arc::clone(&release);
    let mut second = factory
        .start(
            second_request.clone(),
            Box::new(move |size| {
                start.wait();
                finish.wait();
                second_launch(size)
            }),
            engine_for(second_pty.clone()),
        )
        .unwrap();
    entered.wait();
    let accepted = first
        .handle()
        .try_submit(fence(first_request.authority()), ime("a"))
        .unwrap();
    let rejected = second
        .handle()
        .try_submit(fence(second_request.authority()), ime("b"))
        .err();
    release.wait();
    first.wait_until_running().unwrap();
    second.wait_until_running().unwrap();
    accepted.receive_blocking().unwrap();
    first.retire(RetirementReason::Stop);
    second.retire(RetirementReason::Stop);
    first.join().unwrap();
    second.join().unwrap();
    assert_eq!(
        rejected.map(|error| error.class),
        Some(FailureClass::ResourceLimit)
    );
    assert!(second_pty.inspect(|state| state.written.is_empty()));
}
#[test]
fn unread_topology_result_resyncs_when_output_changes_before_delivery() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    let old = stamp(request.authority(), 1, 8, 2);
    let reply = owner.handle().read_topology(topology(old)).unwrap();
    pty.until(|state| state.content_reads == 1);
    pty.output(b"new output");
    pty.until(|state| state.engine_bytes == b"new output");
    owner
        .handle()
        .try_submit(fence(request.authority()), ime("barrier"))
        .unwrap()
        .receive_blocking()
        .unwrap();
    let result = reply.receive_blocking().unwrap();
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
    assert!(
        matches!(result, spaceterm_core::content_queries::ContentReadResult::ResyncRequired { request, current } if request == old && current.revision == ContentRevision::new(2))
    );
}
#[test]
fn resource_cancel_never_delivers_buffered_results_as_complete() {
    use spaceterm_core::content_queries::*;
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    let query = FindRequest::new(
        stamp(request.authority(), 1, 2, 1),
        BoundedFindQuery::new("query".into()).unwrap(),
        None,
    )
    .unwrap();
    let mut stream = owner.handle().find(query).unwrap();
    pty.until(|state| state.content_steps == 2);
    stream.cancel();
    assert!(stream.next_blocking().unwrap().is_none());
    owner
        .handle()
        .try_submit(fence(request.authority()), ime("alive"))
        .unwrap()
        .receive_blocking()
        .unwrap();
    assert_eq!(pty.inspect(|state| state.written.clone()), b"alive");
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}

#[test]
fn application_pointer_and_wheel_reject_a_different_screen_or_topology_before_effect() {
    use spaceterm_core::content::{CellRef, RowId, Screen};
    use spaceterm_core::content_queries::QueryStamp;
    use spaceterm_terminal::pointer_input::{PointerButton, PointerPhase};
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    let current = stamp(request.authority(), 1, 1, 1);
    for base in [
        spaceterm_core::content::ContentBase {
            screen: Screen::Alternate,
            ..current.base()
        },
        spaceterm_core::content::ContentBase {
            screen_generation: 2,
            ..current.base()
        },
        spaceterm_core::content::ContentBase {
            topology_revision: 2,
            ..current.base()
        },
    ] {
        let stamp = QueryStamp::new(
            current.fence(),
            base,
            current.request(),
            current.generation(),
        )
        .unwrap();
        let cell = CellRef {
            session: base.session,
            row: RowId {
                screen: base.screen,
                screen_generation: base.screen_generation,
                node_serial: 1,
                page_row: 0,
            },
            row_revision: 1,
            column: 0,
        };
        for command in [
            RuntimeCommand::Pointer(ApplicationPointerInput {
                stamp,
                cell,
                phase: PointerPhase::Press,
                button: Some(PointerButton::Left),
                modifiers: Default::default(),
            }),
            RuntimeCommand::Wheel(ApplicationWheelInput {
                stamp,
                cell,
                horizontal_steps: 0,
                vertical_steps: 1,
                modifiers: Default::default(),
            }),
        ] {
            let result = owner.handle().try_submit(current.fence(), command);
            assert_eq!(
                result.err().map(|error| error.class),
                Some(FailureClass::StaleAuthority)
            );
        }
    }
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}

#[test]
fn resource_admission_compares_the_complete_content_base() {
    use spaceterm_core::content_queries::*;
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    let current = stamp(request.authority(), 1, 1, 1);
    let wrong = QueryStamp::new(
        current.fence(),
        spaceterm_core::content::ContentBase {
            topology_revision: 2,
            ..current.base()
        },
        current.request(),
        current.generation(),
    )
    .unwrap();
    let query =
        FindRequest::new(wrong, BoundedFindQuery::new("query".into()).unwrap(), None).unwrap();
    let result = owner.handle().find(query);
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
    assert_eq!(
        result.err().map(|error| error.class),
        Some(FailureClass::StaleAuthority)
    );
    assert_eq!(pty.inspect(|state| state.content_steps), 0);
}

#[test]
fn focus_restoration_cannot_reauthorize_input_queued_before_focus_loss() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    pty.update(|state| state.poll_block_at = Some(1));
    let mut owner = running(&factory, request.clone(), &pty);
    pty.until(|state| state.poll_count == 1);
    let mut queued = Vec::new();
    for _ in 0..40 {
        queued.push(
            owner
                .handle()
                .try_submit(fence(request.authority()), ime("old"))
                .unwrap(),
        );
    }
    let mut parts = request.authority().parts();
    parts.focused_pane = false;
    parts.terminal_input_focus = false;
    parts.native_effects_eligible = false;
    let unfocused = RuntimeAuthority::new(parts).unwrap();
    let transition = RuntimeAuthorityTransition::new(request.authority(), unfocused).unwrap();
    let loss = owner.transition_authority(transition).unwrap();
    // Keep more than one scheduling quantum of pre-loss commands queued.
    pty.update(|state| state.poll_block_at = Some(2));
    loss.receive_blocking().unwrap();
    pty.until(|state| state.poll_count == 2);
    let restored = owner
        .transition_authority(
            RuntimeAuthorityTransition::new(unfocused, request.authority()).unwrap(),
        )
        .unwrap();
    pty.update(|state| state.poll_block_at = None);
    restored.receive_blocking().unwrap();
    let dispositions: Vec<_> = queued
        .into_iter()
        .map(|reply| reply.receive_blocking().unwrap())
        .collect();
    owner
        .handle()
        .try_submit(fence(request.authority()), ime("fresh"))
        .unwrap()
        .receive_blocking()
        .unwrap();
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
    assert_eq!(dispositions, vec![EffectDisposition::StaleAuthority; 40]);
    assert_eq!(pty.inspect(|state| state.written.clone()), b"fresh");
}

#[test]
fn delayed_content_and_presentation_cannot_reacquire_restored_authority() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    let mut presentation = owner.subscribe(fence(request.authority())).unwrap();
    let result = owner
        .handle()
        .read_topology(topology(stamp(request.authority(), 1, 1, 1)))
        .unwrap();
    pty.until(|state| state.content_reads == 1);
    let mut parts = request.authority().parts();
    parts.focused_pane = false;
    parts.terminal_input_focus = false;
    parts.native_effects_eligible = false;
    let unfocused = RuntimeAuthority::new(parts).unwrap();
    owner
        .transition_authority(
            RuntimeAuthorityTransition::new(request.authority(), unfocused).unwrap(),
        )
        .unwrap()
        .receive_blocking()
        .unwrap();
    owner
        .transition_authority(
            RuntimeAuthorityTransition::new(unfocused, request.authority()).unwrap(),
        )
        .unwrap()
        .receive_blocking()
        .unwrap();
    let content = result.receive_blocking();
    let notification = presentation.next_blocking();
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
    assert_eq!(
        content.err().map(|error| error.class),
        Some(FailureClass::StaleAuthority)
    );
    assert_eq!(
        notification.err().map(|error| error.class),
        Some(FailureClass::StaleAuthority)
    );
}

#[test]
fn wheel_admission_bounds_both_axes_and_reserves_worst_case_encoded_input() {
    use spaceterm_core::content::{CellRef, RowId};
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(512, 1024));
    let pty = Pty::default();
    // Two 100-step axes at 8K pixel coordinates can exceed the old 4 KiB claim.
    pty.update(|state| state.wheel_bytes = Some(4200));
    let mut owner = running(&factory, request.clone(), &pty);
    let stamp = stamp(request.authority(), 1, 1, 1);
    let base = stamp.base();
    let result = owner
        .handle()
        .try_submit(
            stamp.fence(),
            RuntimeCommand::Wheel(ApplicationWheelInput {
                stamp,
                cell: CellRef {
                    session: base.session,
                    row: RowId {
                        screen: base.screen,
                        screen_generation: base.screen_generation,
                        node_serial: 1,
                        page_row: 0,
                    },
                    row_revision: 1,
                    column: 0,
                },
                horizontal_steps: i32::MAX,
                vertical_steps: i32::MIN,
                modifiers: Default::default(),
            }),
        )
        .unwrap()
        .receive_blocking();
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
    assert_eq!(result.unwrap(), EffectDisposition::Applied);
    assert_eq!(
        pty.inspect(|state| state.wheel_steps.clone()),
        vec![(100, -100)]
    );
    assert_eq!(pty.inspect(|state| state.written.len()), 4200);
}
