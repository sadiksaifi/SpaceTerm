#![cfg(feature = "runtime")]
mod support;
use spaceterm_core::{organization::RetirementReason, retirement::CleanupResult};
use spaceterm_terminal::runtime::budget::RuntimeLimits;
use spaceterm_terminal::runtime_contracts::{RuntimeCommand, RuntimeLifecycle};
use support::*;

#[test]
fn dropping_all_handles_and_subscriptions_keeps_session_running() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    drop(owner.handle());
    drop(owner.subscribe(fence(request.authority())).unwrap());
    assert_eq!(owner.lifecycle_snapshot(), RuntimeLifecycle::Running);
    assert_eq!(pty.inspect(|state| state.terminated), 0);
    let reply = owner
        .handle()
        .try_submit(
            fence(request.authority()),
            RuntimeCommand::InputMethodCommit(
                spaceterm_terminal::runtime_contracts::BoundedInputMethodText::new(
                    "still running".into(),
                )
                .unwrap(),
            ),
        )
        .unwrap();
    assert_eq!(
        reply.receive_blocking().unwrap(),
        spaceterm_terminal::runtime_contracts::EffectDisposition::Applied
    );
    assert_eq!(pty.inspect(|state| state.written.clone()), b"still running");
    owner.retire(RetirementReason::EndAllSessions);
    assert!(matches!(
        owner.join().unwrap().result(),
        CleanupResult::Confirmed(_)
    ));
}

#[test]
fn new_subscription_observes_same_session_after_subscriber_loss() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    drop(owner.subscribe(fence(request.authority())).unwrap());
    let mut replacement = owner.subscribe(fence(request.authority())).unwrap();
    assert_eq!(
        replacement
            .next_blocking()
            .unwrap()
            .unwrap()
            .fence
            .session(),
        request.reservation().session()
    );
    assert_eq!(pty.inspect(|state| state.launch_count), 1);
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
}
#[test]
fn ordinary_exit_is_retained_without_any_subscriber() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request, &pty);
    pty.update(|state| state.eof = true);
    let cleanup = owner.join().unwrap();
    assert_eq!(cleanup.first_reason(), RetirementReason::OrdinaryExit);
    assert!(matches!(
        owner
            .lifecycle_snapshot()
            .terminal_result()
            .unwrap()
            .status(),
        spaceterm_terminal::runtime_contracts::RuntimeTerminalStatus::OrdinaryExit {
            exit_status: Some(0)
        }
    ));
}
#[test]
fn retirement_waits_for_process_reader_and_termination_cleanup() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    pty.update(|state| state.termination_blocked = true);
    let mut owner = running(&factory, request, &pty);
    let ticket = owner.retire(RetirementReason::Stop);
    pty.until(|state| state.terminated == 1);
    assert!(ticket.snapshot().is_none());
    assert!(matches!(
        owner.lifecycle_snapshot(),
        RuntimeLifecycle::Retiring { .. }
    ));
    pty.update(|state| state.termination_blocked = false);
    assert!(matches!(
        owner.join().unwrap().result(),
        CleanupResult::Confirmed(_)
    ));
    assert!(pty.inspect(|state| state.reader_dropped
        && state.process_dropped
        && state.termination_completed
        && state.engine_dropped));
}
#[test]
fn repeated_retirement_preserves_first_reason_and_one_cleanup() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request, &pty);
    let first = owner.retire(RetirementReason::WorktreeRemoval);
    let second = owner.retire(RetirementReason::Stop);
    let cleanup = owner.join().unwrap();
    assert_eq!(cleanup.first_reason(), RetirementReason::WorktreeRemoval);
    assert_eq!(
        first.wait_blocking().unwrap(),
        second.wait_blocking().unwrap()
    );
    assert_eq!(pty.inspect(|state| state.terminated), 1);
}
#[test]
fn cleanup_timeout_never_reports_retired_success_or_allows_replacement() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    pty.update(|state| state.wait_timeout = true);
    let mut owner = running(&factory, request.clone(), &pty);
    owner.retire(RetirementReason::EndAllSessions);
    assert!(matches!(
        owner.join().unwrap().result(),
        CleanupResult::Failed(_)
    ));
    let replacement = Pty::default();
    assert!(
        factory
            .start(request, replacement.launch(), engine())
            .is_err()
    );
    assert_eq!(replacement.inspect(|state| state.launch_count), 0);
}
#[test]
fn startup_failure_after_process_creation_cleans_every_owned_resource() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    pty.update(|state| state.reader_failure = true);
    let mut owner = factory
        .start(request, pty.launch(), engine_for(pty.clone()))
        .unwrap();
    assert!(matches!(
        owner.join().unwrap().result(),
        CleanupResult::Confirmed(_)
    ));
    assert!(matches!(
        owner
            .lifecycle_snapshot()
            .terminal_result()
            .unwrap()
            .status(),
        spaceterm_terminal::runtime_contracts::RuntimeTerminalStatus::StartupFailed { .. }
    ));
    assert!(
        pty.inspect(|state| state.process_dropped && state.engine_dropped && state.terminated == 1)
    );
}
#[test]
fn retirement_before_pty_installation_cleans_started_process() {
    use std::sync::{Arc, Barrier};
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let launch = pty.launch();
    let start = Arc::clone(&entered);
    let finish = Arc::clone(&release);
    let mut owner = factory
        .start(
            request,
            Box::new(move |size| {
                start.wait();
                finish.wait();
                launch(size)
            }),
            engine_for(pty.clone()),
        )
        .unwrap();
    entered.wait();
    owner.retire(RetirementReason::EndAllSessions);
    release.wait();
    assert_eq!(
        owner.join().unwrap().first_reason(),
        RetirementReason::EndAllSessions
    );
    assert!(
        pty.inspect(|state| state.reader_dropped && state.process_dropped && state.terminated == 1)
    );
}
#[test]
fn classified_engine_failure_is_not_hidden_by_successful_cleanup() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request, &pty);
    pty.output(b"engine-failure");
    assert!(matches!(
        owner.join().unwrap().result(),
        CleanupResult::Confirmed(_)
    ));
    assert_eq!(
        owner
            .lifecycle_snapshot()
            .terminal_result()
            .unwrap()
            .status(),
        spaceterm_terminal::runtime_contracts::RuntimeTerminalStatus::RuntimeFailed {
            class: spaceterm_core::organization::FailureClass::ResourceLimit
        }
    );
}
#[test]
fn emergency_owner_drop_joins_cleanup_but_handle_drop_does_not() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let owner = running(&factory, request.clone(), &pty);
    let handle = owner.handle();
    drop(owner);
    assert!(
        pty.inspect(|state| state.reader_dropped && state.process_dropped && state.terminated == 1)
    );
    assert!(
        handle
            .try_submit(fence(request.authority()), ime("ended"))
            .is_err()
    );
}

#[test]
fn retained_observation_snapshots_preserve_source_ticks_and_independent_watermarks() {
    use spaceterm_core::{
        authority::CloseWorkGeneration, observations::*, organization::ServerDirectory,
    };
    use spaceterm_terminal::{
        metadata::*,
        runtime::{owner::RuntimeFactory, ports::EngineObservations},
        runtime_contracts::*,
    };
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    let clock = Arc::new(Clock::default());
    let factory = RuntimeFactory::new(RuntimeLimits::default(), clock.clone()).unwrap();
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let engine = engine_with_observations(pty.clone(), |context| {
        let session = context.session;
        let server = session.pane.server();
        EngineObservations {
            metadata: Some(
                RuntimeMetadataSnapshot::new(RuntimeMetadataParts {
                    session,
                    sequence: RuntimeObservationSequence::new(1).unwrap(),
                    observed_at: MonotonicTick::new(5),
                    title: TitleMetadata {
                        value: Arc::from("bounded title"),
                        provenance: TitleProvenance::TerminalControl,
                    },
                    origin: RuntimeMetadataOrigin::Local {
                        user: None,
                        host: None,
                    },
                    progress: ProgressMetadata::Normal(42),
                    freshness: MetadataFreshness::Live,
                    prompt_zone: PromptZone::Prompt,
                    hidden_input: false,
                })
                .unwrap(),
            ),
            display_directory: Some(
                DisplayDirectoryObservation::new(
                    session,
                    RuntimeObservationSequence::new(10).unwrap(),
                    MonotonicTick::new(4),
                    ServerDirectory::new(server, b"/display-only".to_vec()).unwrap(),
                    DirectoryPresentation::OwningServer,
                )
                .unwrap(),
            ),
            confirmed_directory: Some(
                ConfirmedDirectoryObservation::new(
                    session,
                    RuntimeObservationSequence::new(1).unwrap(),
                    MonotonicTick::new(2),
                    ServerDirectory::new(server, b"/confirmed".to_vec()).unwrap(),
                )
                .unwrap(),
            ),
            close_work: Some(
                CloseWorkObservation::new(
                    session,
                    RuntimeObservationSequence::new(2).unwrap(),
                    CloseWorkGeneration::new(1),
                    MonotonicTick::new(3),
                    CloseWorkState::Idle,
                )
                .unwrap(),
            ),
        }
    });
    let mut owner = factory
        .start(request.clone(), pty.launch(), engine)
        .unwrap();
    owner.wait_until_running().unwrap();
    // A command completion is a worker barrier after its first observation poll.
    owner
        .handle()
        .try_submit(fence(request.authority()), ime("barrier"))
        .unwrap()
        .receive_blocking()
        .unwrap();
    assert_eq!(
        owner.metadata_snapshot().unwrap().parts().observed_at,
        MonotonicTick::new(5)
    );
    let display = owner.display_directory_snapshot().unwrap();
    let confirmed = owner.confirmed_directory_snapshot().unwrap();
    assert_eq!(display.sequence().get(), 10);
    assert_eq!(confirmed.sequence().get(), 1);
    assert_eq!(confirmed.observed_at(), MonotonicTick::new(2));
    assert_eq!(confirmed.directory().bytes(), b"/confirmed");
    clock.0.store(100, Ordering::Release);
    let work = owner.close_work_snapshot().unwrap();
    assert_eq!(work.observed_at(), MonotonicTick::new(3));
    assert!(!work.is_fresh(MonotonicTick::new(100), 10));
    owner.retire(RetirementReason::EndAllSessions);
    owner.join().unwrap();
    assert!(owner.metadata_snapshot().is_none());
    assert!(owner.confirmed_directory_snapshot().is_none());
    assert!(owner.display_directory_snapshot().is_none());
    assert!(owner.close_work_snapshot().is_none());
    assert!(pty.inspect(|state| state.engine_dropped));
}

#[test]
fn retirement_clears_engine_content_before_waiting_for_native_cleanup() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    pty.update(|state| state.termination_blocked = true);
    let mut owner = running(&factory, request, &pty);
    owner.retire(RetirementReason::EndAllSessions);
    pty.until(|state| state.terminated == 1);
    let content_cleared = pty.inspect(|state| state.engine_dropped);
    pty.update(|state| state.termination_blocked = false);
    owner.join().unwrap();
    assert!(content_cleared);
}

#[test]
fn read_failure_is_classified_independently_from_confirmed_resource_cleanup() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request, &pty);
    pty.update(|state| state.read_io_failure = true);
    assert!(matches!(
        owner.join().unwrap().result(),
        CleanupResult::Confirmed(_)
    ));
    let result = owner.lifecycle_snapshot().terminal_result().unwrap();
    assert_eq!(
        result.status(),
        spaceterm_terminal::runtime_contracts::RuntimeTerminalStatus::RuntimeFailed {
            class: spaceterm_core::organization::FailureClass::RuntimeFault
        }
    );
    assert!(!format!("{result:?}").contains("native-error-sensitive-sentinel"));
    assert!(pty.inspect(|state| state.reader_dropped && state.process_dropped));
}
#[test]
fn terminal_reply_capacity_exhaustion_is_a_retained_runtime_failure_not_silent_loss() {
    let factory = factory(RuntimeLimits {
        pending_input_bytes: 65_536 + 1024,
        ..RuntimeLimits::default()
    });
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request, &pty);
    pty.update(|state| state.reply_bytes = Some(65_536 + 1025));
    pty.output(b"query");
    assert!(matches!(
        owner.join().unwrap().result(),
        CleanupResult::Confirmed(_)
    ));
    assert_eq!(
        owner
            .lifecycle_snapshot()
            .terminal_result()
            .unwrap()
            .status(),
        spaceterm_terminal::runtime_contracts::RuntimeTerminalStatus::RuntimeFailed {
            class: spaceterm_core::organization::FailureClass::ResourceLimit
        }
    );
    assert!(pty.inspect(|state| state.written.is_empty()));
}

#[test]
fn authority_loss_during_engine_construction_cancels_the_original_startup() {
    use spaceterm_terminal::runtime_contracts::{
        RuntimeAuthority, RuntimeAuthorityTransition, RuntimeTerminalStatus,
    };
    use std::sync::{Arc, Barrier};
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let start = Arc::clone(&entered);
    let finish = Arc::clone(&release);
    // Block before an engine exists. No final-effect lock is held here.
    let delayed = engine_after(pty.clone(), move || {
        start.wait();
        finish.wait();
    });
    let mut owner = factory
        .start(request.clone(), pty.launch(), delayed)
        .unwrap();
    entered.wait();
    let mut parts = request.authority().parts();
    parts.focused_pane = false;
    parts.terminal_input_focus = false;
    parts.native_effects_eligible = false;
    let unfocused = RuntimeAuthority::new(parts).unwrap();
    let loss = owner
        .transition_authority(
            RuntimeAuthorityTransition::new(request.authority(), unfocused).unwrap(),
        )
        .unwrap();
    release.wait();
    let startup = owner.wait_until_running();
    if startup.is_ok() {
        owner.retire(RetirementReason::Stop);
    }
    let cleanup = owner.join().unwrap();
    let _ = loss.receive_blocking();
    assert!(startup.is_err());
    assert_eq!(cleanup.first_reason(), RetirementReason::RuntimeFailure);
    assert_eq!(
        owner
            .lifecycle_snapshot()
            .terminal_result()
            .unwrap()
            .status(),
        RuntimeTerminalStatus::StartupFailed {
            class: spaceterm_core::organization::FailureClass::StaleAuthority
        }
    );
    assert_eq!(pty.inspect(|state| state.launch_count), 0);
}

#[test]
fn coalesced_presentation_keeps_unread_metadata_invalidation() {
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let mut owner = running(&factory, request.clone(), &pty);
    let mut presentation = owner.subscribe(fence(request.authority())).unwrap();
    pty.output(b"new content");
    pty.until(|state| state.engine_bytes == b"new content");
    owner
        .handle()
        .try_submit(fence(request.authority()), ime("barrier"))
        .unwrap()
        .receive_blocking()
        .unwrap();
    let update = presentation.next_blocking().unwrap().unwrap();
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
    assert_eq!(
        update.revision,
        spaceterm_core::authority::ContentRevision::new(2)
    );
    assert!(update.metadata_changed);
}

#[test]
fn native_hidden_input_is_supplied_to_the_worker_local_metadata_producer() {
    use spaceterm_core::observations::*;
    use spaceterm_terminal::{
        metadata::*, runtime::ports::EngineObservations, runtime_contracts::*,
    };
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    pty.update(|state| state.hidden = true);
    let engine = engine_with_observations(pty.clone(), |context| EngineObservations {
        metadata: Some(
            RuntimeMetadataSnapshot::new(RuntimeMetadataParts {
                session: context.session,
                sequence: RuntimeObservationSequence::new(1).unwrap(),
                observed_at: MonotonicTick::new(0),
                title: TitleMetadata {
                    value: std::sync::Arc::from(""),
                    provenance: TitleProvenance::TerminalControl,
                },
                origin: RuntimeMetadataOrigin::Local {
                    user: None,
                    host: None,
                },
                progress: ProgressMetadata::None,
                freshness: MetadataFreshness::Live,
                prompt_zone: PromptZone::Prompt,
                hidden_input: false,
            })
            .unwrap(),
        ),
        ..EngineObservations::default()
    });
    let mut owner = factory
        .start(request.clone(), pty.launch(), engine)
        .unwrap();
    owner.wait_until_running().unwrap();
    owner
        .handle()
        .try_submit(fence(request.authority()), ime("barrier"))
        .unwrap()
        .receive_blocking()
        .unwrap();
    let metadata = owner.metadata_snapshot().unwrap();
    owner.retire(RetirementReason::Stop);
    owner.join().unwrap();
    assert!(metadata.parts().hidden_input);
}

#[test]
fn panicking_launch_without_returned_ownership_never_proves_process_absence() {
    use spaceterm_terminal::runtime::ports::PtyParts;
    let factory = factory(RuntimeLimits::default());
    let request = request(1, grid(24, 80));
    let pty = Pty::default();
    let launch = pty.launch();
    let mut owner = factory
        .start(
            request.clone(),
            Box::new(move |size| {
                let _unreturned_parts: PtyParts = launch(size)?;
                panic!("injected launch adapter unwind");
            }),
            engine_for(pty.clone()),
        )
        .unwrap();
    let cleanup = owner.join().unwrap();
    let replacement_pty = Pty::default();
    let replacement = factory.start(request, replacement_pty.launch(), engine());
    let admitted = replacement.is_ok();
    if let Ok(mut replacement) = replacement {
        replacement.retire(RetirementReason::Stop);
        replacement.join().unwrap();
    }
    assert!(matches!(cleanup.result(), CleanupResult::Failed(_)));
    assert!(!admitted);
    assert_eq!(pty.inspect(|state| state.terminated), 0);
}
