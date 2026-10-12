//! Pane presentation policy against real views, validated core snapshots and
//! the explicit-use state.

use spaceterm_core::authority::{
    ConnectionBinding, ConnectionGeneration, SessionGeneration, SessionKey,
};
use spaceterm_core::control::StartupSource;
use spaceterm_core::organization::{ControlRecord, EndReason, FailureClass, SessionPhase};

use super::startup::{
    OPEN, RETRY, SELECT, START, World, ended_held, grid, held, intent, running, target, ticket,
};
use super::{
    A, Runtime, attach, client, observe, offline, pairing, pane, ready, ready_client, reconcile,
    synchronizing, tab, ws,
};
use crate::view::{
    ArrangementEdit, ClientViewState, ContentDisposition, InvalidationReason, OfflinePrior,
    PanePresentation, PresentationDecision, PresentationEligibility, PresentationFacts,
    PresentationState, ReplicaFacts, ReplicaReadiness, RestorationFilter, ServerViewStatus,
    StartupEffect, StartupIntents, StartupState, UseKind, ViewError,
};

const NONE: PresentationEligibility = PresentationEligibility {
    paint: false,
    accessibility: false,
    copy: false,
    ime: false,
    input: false,
    native_effects: false,
};

const LIVE: PresentationEligibility = PresentationEligibility {
    paint: true,
    accessibility: true,
    copy: true,
    ime: true,
    input: true,
    native_effects: true,
};

const OFFLINE_FROZEN: PresentationEligibility = PresentationEligibility {
    paint: true,
    accessibility: true,
    ..NONE
};

const RECONCILING_FROZEN: PresentationEligibility = PresentationEligibility {
    paint: true,
    ..NONE
};

fn key(world: &World, pane_local: u64) -> SessionKey {
    let snapshot = world.org.snapshot();
    let record = snapshot
        .runtime()
        .iter()
        .find(|record| record.pane == pane(A, pane_local))
        .unwrap();
    SessionKey {
        epoch: snapshot.epoch(),
        pane: pane(A, pane_local),
        generation: record.session_generation,
    }
}

fn present(
    world: &World,
    state: &mut PresentationState,
    intents: &StartupIntents,
    pane_local: u64,
    displayed: Option<SessionKey>,
) -> PresentationDecision {
    world.with(|facts| {
        let facts = PresentationFacts::observe(
            facts.view,
            facts.replica,
            ws(A, 1),
            pane(A, pane_local),
            intents,
            displayed,
        )
        .unwrap();
        state.reduce(&facts).unwrap()
    })
}

/// A live Pane 1 whose Session the projection displays.
fn live(world: &mut World, state: &mut PresentationState) -> SessionKey {
    world.set(1, running(1, Some(1), 1));
    let session = key(world, 1);
    let decision = present(world, state, &StartupIntents::new(), 1, None);
    assert_eq!(decision.presentation, PanePresentation::Live);
    assert_eq!(decision.eligibility, LIVE);
    assert_eq!(decision.content, ContentDisposition::Allow { session });
    assert_eq!(decision.controller, None);
    assert!(!decision.clear_content);
    assert!(!decision.invalidate_native_context);
    assert!(decision.presentation.accepts_input());
    assert!(decision.presentation.content_eligible());
    session
}

fn assert_content_free(decision: &PresentationDecision) {
    assert_eq!(decision.eligibility, NONE);
    assert_eq!(decision.content, ContentDisposition::Clear);
    assert!(!decision.presentation.accepts_input());
}

#[test]
fn takeover_disables_paint_accessibility_copy_ime_input_and_native_effects() {
    let mut world = World::new();
    let mut state = PresentationState::new();
    let session = live(&mut world, &mut state);

    world.set(1, running(1, Some(2), 2));
    let takeover = present(&world, &mut state, &StartupIntents::new(), 1, Some(session));
    assert_eq!(takeover.presentation, PanePresentation::Takeover);
    assert_eq!(takeover.controller, Some(client(A, 2)));
    let PresentationEligibility {
        paint,
        accessibility,
        copy,
        ime,
        input,
        native_effects,
    } = takeover.eligibility;
    assert!(!paint && !accessibility && !copy && !ime && !input && !native_effects);
    assert_content_free(&takeover);
    assert!(!takeover.presentation.content_eligible());
    assert!(takeover.clear_content);
    assert!(takeover.invalidate_native_context);

    // An unclaimed running Pane shows no content before acquisition, also
    // while this Client's compare-acquire is in flight.
    world.set(1, running(1, None, 3));
    let mut intents = StartupIntents::new();
    let acquire = world.begin(&mut intents, target(1, 1), SELECT).unwrap();
    assert!(matches!(
        acquire.effects[..],
        [StartupEffect::AcquireControl { .. }]
    ));
    let unclaimed = present(&world, &mut state, &intents, 1, None);
    assert_eq!(
        unclaimed.presentation,
        PanePresentation::Unclaimed { acquiring: true }
    );
    assert_eq!(unclaimed.controller, None);
    assert_content_free(&unclaimed);
    assert!(!unclaimed.clear_content);
    assert!(!unclaimed.invalidate_native_context);
}

#[test]
fn offline_takeover_never_reveals_previous_terminal_frame() {
    let mut world = World::new();
    let mut state = PresentationState::new();
    let session = live(&mut world, &mut state);
    world.set(1, running(1, Some(2), 2));
    present(&world, &mut state, &StartupIntents::new(), 1, Some(session));

    // The projection still holds the old frame while the Server is offline.
    world.facts = offline(A);
    let offline_takeover = present(&world, &mut state, &StartupIntents::new(), 1, Some(session));
    assert_eq!(
        offline_takeover.presentation,
        PanePresentation::Offline {
            prior: OfflinePrior::Takeover
        }
    );
    assert_eq!(offline_takeover.controller, Some(client(A, 2)));
    assert_content_free(&offline_takeover);
    assert!(!offline_takeover.presentation.content_eligible());
    assert!(offline_takeover.clear_content);
    assert!(offline_takeover.invalidate_native_context);

    // The screen of an unclaimed Pane is kept while unreachable and while
    // reconciling.
    let mut world = World::new();
    let mut state = PresentationState::new();
    let session = live(&mut world, &mut state);
    world.set(1, running(1, None, 2));
    let unclaimed = present(&world, &mut state, &StartupIntents::new(), 1, Some(session));
    assert_eq!(
        unclaimed.presentation,
        PanePresentation::Unclaimed { acquiring: false }
    );
    assert!(unclaimed.clear_content);
    for (facts, presentation) in [
        (
            offline(A),
            PanePresentation::Offline {
                prior: OfflinePrior::Unclaimed,
            },
        ),
        (
            ReplicaFacts {
                status: ServerViewStatus::Connecting,
                ..offline(A)
            },
            PanePresentation::Offline {
                prior: OfflinePrior::Unclaimed,
            },
        ),
        (
            synchronizing(A),
            PanePresentation::Reconciling {
                prior: OfflinePrior::Unclaimed,
            },
        ),
    ] {
        world.facts = facts;
        let decision = present(&world, &mut state, &StartupIntents::new(), 1, Some(session));
        assert_eq!(decision.presentation, presentation);
        assert_eq!(decision.controller, None);
        assert_content_free(&decision);
    }

    // A Pane first presented while interrupted shows the retained replica's
    // control screen.
    let mut world = World::new();
    world.facts = offline(A);
    for (runtime, prior, controller) in [
        (running(1, None, 2), OfflinePrior::Unclaimed, None),
        (
            running(1, Some(2), 3),
            OfflinePrior::Takeover,
            Some(client(A, 2)),
        ),
    ] {
        world.set(1, runtime);
        let fresh = present(
            &world,
            &mut PresentationState::new(),
            &StartupIntents::new(),
            1,
            None,
        );
        assert_eq!(fresh.presentation, PanePresentation::Offline { prior });
        assert_eq!(fresh.controller, controller);
        assert_content_free(&fresh);
    }
}

#[test]
fn offline_live_screen_is_frozen_and_accepts_no_input() {
    let mut world = World::new();
    let mut state = PresentationState::new();
    let session = live(&mut world, &mut state);

    world.facts = offline(A);
    let frozen = present(&world, &mut state, &StartupIntents::new(), 1, Some(session));
    assert_eq!(
        frozen.presentation,
        PanePresentation::Offline {
            prior: OfflinePrior::Frozen
        }
    );
    assert_eq!(frozen.content, ContentDisposition::KeepFrozen { session });
    assert_eq!(frozen.eligibility, OFFLINE_FROZEN);
    assert!(!frozen.presentation.accepts_input());
    assert!(frozen.presentation.content_eligible());
    // Input, IME, copy and native effects were revoked; the frame stays.
    assert!(frozen.invalidate_native_context);
    assert!(!frozen.clear_content);

    let connecting = ReplicaFacts {
        status: ServerViewStatus::Connecting,
        ..offline(A)
    };
    world.facts = connecting;
    let still = present(&world, &mut state, &StartupIntents::new(), 1, Some(session));
    assert_eq!(still.presentation, frozen.presentation);
    assert_eq!(still.content, ContentDisposition::KeepFrozen { session });
    assert!(!still.clear_content);
    assert!(!still.invalidate_native_context);

    // Without a displayed frame there is nothing to freeze.
    let nothing = present(&world, &mut state, &StartupIntents::new(), 1, None);
    assert_eq!(nothing.presentation, frozen.presentation);
    assert_content_free(&nothing);
    assert!(nothing.clear_content);
}

#[test]
fn connected_socket_without_reconciliation_is_not_live() {
    let mut world = World::new();
    world.set(1, running(1, Some(1), 1));
    let socket = ReplicaFacts {
        readiness: ReplicaReadiness::Retained,
        ..ready(A)
    };
    let mut state = PresentationState::new();
    for facts in [socket, synchronizing(A)] {
        world.facts = facts;
        let decision = present(&world, &mut state, &StartupIntents::new(), 1, None);
        assert_eq!(
            decision.presentation,
            PanePresentation::Reconciling {
                prior: OfflinePrior::Frozen
            }
        );
        assert_content_free(&decision);
    }
    world.facts = ready(A);
    let reconciled = present(&world, &mut state, &StartupIntents::new(), 1, None);
    assert_eq!(reconciled.presentation, PanePresentation::Live);

    // A live frame stays painted, read-only and inaccessible while reconciling.
    let session = key(&world, 1);
    world.facts = synchronizing(A);
    let reconciling = present(&world, &mut state, &StartupIntents::new(), 1, Some(session));
    assert_eq!(
        reconciling.presentation,
        PanePresentation::Reconciling {
            prior: OfflinePrior::Frozen
        }
    );
    assert_eq!(reconciling.eligibility, RECONCILING_FROZEN);
    assert_eq!(
        reconciling.content,
        ContentDisposition::KeepFrozen { session }
    );
    assert!(!reconciling.clear_content);
    assert!(reconciling.invalidate_native_context);
}

#[test]
fn reconciled_takeover_clears_frozen_content() {
    let mut world = World::new();
    let mut state = PresentationState::new();
    let session = live(&mut world, &mut state);
    world.facts = offline(A);
    present(&world, &mut state, &StartupIntents::new(), 1, Some(session));
    world.facts = synchronizing(A);
    let reconciling = present(&world, &mut state, &StartupIntents::new(), 1, Some(session));
    assert_eq!(
        reconciling.content,
        ContentDisposition::KeepFrozen { session }
    );

    world.facts = ready(A);
    world.set(1, running(1, Some(2), 2));
    let takeover = present(&world, &mut state, &StartupIntents::new(), 1, Some(session));
    assert_eq!(takeover.presentation, PanePresentation::Takeover);
    assert_eq!(takeover.controller, Some(client(A, 2)));
    assert_content_free(&takeover);
    assert!(takeover.clear_content);
    assert!(takeover.invalidate_native_context);

    // A later disconnect cannot return to the earlier frame.
    world.facts = offline(A);
    world.set(1, running(1, Some(1), 3));
    let offline_again = present(&world, &mut state, &StartupIntents::new(), 1, Some(session));
    assert_eq!(
        offline_again.presentation,
        PanePresentation::Offline {
            prior: OfflinePrior::Takeover
        }
    );
    assert_content_free(&offline_again);
}

#[test]
fn reconciled_end_all_sessions_clears_content_but_preserves_arrangement() {
    let mut world = World::new();
    world.set(2, running(1, Some(1), 1));
    world.focus(1, 2);
    world
        .view
        .arrange(ws(A, 1), tab(A, 1), ArrangementEdit::ToggleZoom)
        .unwrap();
    let mut state = PresentationState::new();
    let session = live(&mut world, &mut state);

    // Another Client's view of the same Panes.
    let other_facts = ready_client(A, 2, 2);
    let mut other = ClientViewState::new();
    attach(&mut other, &world.org, other_facts, 1).unwrap();
    let mut other_state = PresentationState::new();
    let other_decision = |world: &World, state: &mut PresentationState, other: &ClientViewState| {
        let snapshot = world.org.snapshot();
        let replica = observe(&snapshot, &[], other_facts);
        let facts = PresentationFacts::observe(
            other,
            &replica,
            ws(A, 1),
            pane(A, 1),
            &StartupIntents::new(),
            None,
        )
        .unwrap();
        state.reduce(&facts).unwrap()
    };
    let before = other_decision(&world, &mut other_state, &other);
    assert_eq!(before.presentation, PanePresentation::Takeover);
    assert_eq!(before.controller, Some(client(A, 1)));

    let workspace = world.view.workspace(ws(A, 1)).unwrap().clone();
    world.set(1, ended_held(1, EndReason::EndAllSessions));
    world.set(2, ended_held(1, EndReason::EndAllSessions));
    world.org.publish();
    let update = reconcile(&mut world.view, &world.org, &[], ready(A)).unwrap();
    assert!(update.is_empty());
    assert_eq!(world.view.workspace(ws(A, 1)).unwrap(), &workspace);

    let ended = present(&world, &mut state, &StartupIntents::new(), 1, Some(session));
    assert_eq!(
        ended.presentation,
        PanePresentation::Ended {
            reason: EndReason::EndAllSessions
        }
    );
    assert_content_free(&ended);
    assert!(ended.clear_content);
    assert!(ended.invalidate_native_context);
    let mut intents = StartupIntents::new();
    assert_eq!(world.revalidate(&mut intents).effects, Vec::new());
    assert_eq!(intents.state(), StartupState::Idle);

    // The other Client keeps its Takeover Screen.
    reconcile(&mut other, &world.org, &[], other_facts).unwrap();
    let after = other_decision(&world, &mut other_state, &other);
    assert_eq!(after.presentation, PanePresentation::Takeover);
    assert_eq!(after.controller, Some(client(A, 1)));
    assert_content_free(&after);
}

#[test]
fn replacement_session_rejects_old_displayed_session_key() {
    let mut world = World::new();
    let mut state = PresentationState::new();
    let old = live(&mut world, &mut state);

    // Synchronization that reveals a replacement never keeps the old frame.
    world.facts = synchronizing(A);
    world.set(1, running(2, Some(1), 1));
    let replacement = key(&world, 1);
    assert_ne!(replacement, old);
    let reconciling = present(&world, &mut state, &StartupIntents::new(), 1, Some(old));
    assert_eq!(
        reconciling.presentation,
        PanePresentation::Reconciling {
            prior: OfflinePrior::Frozen
        }
    );
    assert_content_free(&reconciling);
    assert!(reconciling.clear_content);
    assert!(reconciling.invalidate_native_context);

    world.facts = ready(A);
    let decision = present(&world, &mut state, &StartupIntents::new(), 1, Some(old));
    assert_eq!(decision.presentation, PanePresentation::Live);
    assert_eq!(
        decision.content,
        ContentDisposition::Allow {
            session: replacement
        }
    );
    assert!(decision.clear_content);
    assert!(decision.invalidate_native_context);
    let settled = present(
        &world,
        &mut state,
        &StartupIntents::new(),
        1,
        Some(replacement),
    );
    assert!(!settled.clear_content);
    assert!(!settled.invalidate_native_context);

    // An old frame is never kept frozen in place of the replacement.
    world.facts = offline(A);
    let frozen = present(&world, &mut state, &StartupIntents::new(), 1, Some(old));
    assert_eq!(
        frozen.presentation,
        PanePresentation::Offline {
            prior: OfflinePrior::Frozen
        }
    );
    assert_content_free(&frozen);
    assert!(frozen.clear_content);
    assert!(frozen.invalidate_native_context);

    // A new Server epoch replaces every Session key.
    world.facts = ready(A);
    present(
        &world,
        &mut state,
        &StartupIntents::new(),
        1,
        Some(replacement),
    );
    world.org.restart();
    let restarted = key(&world, 1);
    assert_ne!(restarted.epoch, replacement.epoch);
    let decision = present(
        &world,
        &mut state,
        &StartupIntents::new(),
        1,
        Some(replacement),
    );
    assert_eq!(
        decision.content,
        ContentDisposition::Allow { session: restarted }
    );
    assert!(decision.clear_content);
    assert!(decision.invalidate_native_context);
}

#[test]
fn old_connection_page_cannot_reenable_live_presentation() {
    let mut world = World::new();
    let mut state = PresentationState::new();
    let session = live(&mut world, &mut state);
    world.facts = offline(A);
    present(&world, &mut state, &StartupIntents::new(), 1, Some(session));

    // A page from the old connection reaches the projection after reconnect.
    let reconnected = ReplicaFacts {
        connection: Some(ConnectionBinding::new(
            client(A, 1),
            ConnectionGeneration::new(2),
        )),
        ..ready(A)
    };
    for facts in [
        ReplicaFacts {
            readiness: ReplicaReadiness::Retained,
            ..reconnected
        },
        ReplicaFacts {
            readiness: ReplicaReadiness::Synchronizing,
            ..reconnected
        },
    ] {
        world.facts = facts;
        let decision = present(&world, &mut state, &StartupIntents::new(), 1, Some(session));
        assert_eq!(
            decision.presentation,
            PanePresentation::Reconciling {
                prior: OfflinePrior::Frozen
            }
        );
        assert!(!decision.presentation.accepts_input());
        assert!(!decision.eligibility.input);
        assert!(!decision.eligibility.ime);
        assert!(!decision.eligibility.native_effects);
        assert!(!decision.eligibility.copy);
    }

    // Reconciliation shows the Session was replaced: the old page is cleared.
    world.facts = reconnected;
    world.set(1, running(2, Some(1), 1));
    let replacement = key(&world, 1);
    let decision = present(&world, &mut state, &StartupIntents::new(), 1, Some(session));
    assert_eq!(decision.presentation, PanePresentation::Live);
    assert_eq!(
        decision.content,
        ContentDisposition::Allow {
            session: replacement
        }
    );
    assert!(decision.clear_content);
    assert!(decision.invalidate_native_context);
}

#[test]
fn directory_unavailable_has_no_fallback_launch() {
    let mut world = World::new();
    world.set(
        1,
        Runtime {
            phase: SessionPhase::DirectoryUnavailable,
            ..ended_held(1, EndReason::ServerRestart)
        },
    );
    let mut state = PresentationState::new();
    let mut intents = StartupIntents::new();
    for _ in 0..3 {
        let decision = present(&world, &mut state, &intents, 1, None);
        assert_eq!(
            decision.presentation,
            PanePresentation::DirectoryUnavailable
        );
        assert_content_free(&decision);
        assert_eq!(world.revalidate(&mut intents).effects, Vec::new());
    }
    for source in [SELECT, OPEN] {
        let reduction = world.begin(&mut intents, target(1, 1), source).unwrap();
        assert_eq!(reduction.effects, Vec::new());
    }
    assert_eq!(
        present(&world, &mut state, &intents, 1, None).presentation,
        PanePresentation::DirectoryUnavailable
    );

    // Retry is the deliberate server Start Session; it carries no directory.
    let retry = world.begin(&mut intents, target(1, 1), RETRY).unwrap();
    let StartupEffect::RequestUse { kind, context, .. } = retry.effects[0] else {
        panic!("Retry requests a use");
    };
    assert_eq!(kind, StartupSource::StartSession);
    assert_eq!(context.phase(), SessionPhase::DirectoryUnavailable);
    let awaiting = present(&world, &mut state, &intents, 1, None);
    assert_eq!(awaiting.presentation, PanePresentation::AwaitingMeasurement);
    assert_content_free(&awaiting);

    // A use begun on an earlier connection shows no startup progress.
    world.facts = ReplicaFacts {
        connection: Some(ConnectionBinding::new(
            client(A, 1),
            ConnectionGeneration::new(2),
        )),
        ..ready(A)
    };
    assert_eq!(
        present(&world, &mut state, &intents, 1, None).presentation,
        PanePresentation::DirectoryUnavailable
    );
    world.facts = ready(A);

    world
        .authorize(&mut intents, ticket(1), intent(1), world.fence(1, 1))
        .unwrap();
    world
        .measure(&mut intents, ticket(1), world.layout(1), grid(24, 80))
        .unwrap();
    assert_eq!(intents.state(), StartupState::Submitted);
    let starting = present(&world, &mut state, &intents, 1, None);
    assert_eq!(starting.presentation, PanePresentation::Starting);
    assert_content_free(&starting);
}

#[test]
fn failed_pane_remains_present_without_automatic_retry() {
    let mut world = World::new();
    let failed = Runtime {
        phase: SessionPhase::Failed {
            class: FailureClass::LaunchUnavailable,
        },
        ..Runtime::default()
    };
    world.set(1, failed);
    world.org.publish();
    let update = reconcile(&mut world.view, &world.org, &[], ready(A)).unwrap();
    assert!(update.is_empty());
    let arrangement = world
        .view
        .workspace(ws(A, 1))
        .unwrap()
        .arrangement(tab(A, 1))
        .unwrap();
    assert!(arrangement.contains_pane(pane(A, 1)));

    let mut state = PresentationState::new();
    let mut intents = StartupIntents::new();
    // Selecting the unclaimed Pane compare-acquires it once and never starts it.
    let selected = world.begin(&mut intents, target(1, 1), SELECT).unwrap();
    assert!(matches!(
        selected.effects[..],
        [StartupEffect::AcquireControl { .. }]
    ));
    for _ in 0..3 {
        let decision = present(&world, &mut state, &intents, 1, None);
        assert_eq!(
            decision.presentation,
            PanePresentation::Failed {
                class: FailureClass::LaunchUnavailable
            }
        );
        assert_eq!(decision.controller, None);
        assert_content_free(&decision);
        assert_eq!(world.revalidate(&mut intents).effects, Vec::new());
        let selected = world.begin(&mut intents, target(1, 1), SELECT).unwrap();
        assert_eq!(selected.effects, Vec::new());
    }

    let retry = world.begin(&mut intents, target(1, 1), RETRY).unwrap();
    assert!(matches!(
        retry.effects[0],
        StartupEffect::RequestUse {
            kind: StartupSource::StartSession,
            ..
        }
    ));
    assert_eq!(intents.pending().unwrap().kind, UseKind::Startup);
}

#[test]
fn detached_or_suppressed_attachment_has_no_native_eligibility() {
    let mut world = World::new();
    let mut state = PresentationState::new();
    let session = live(&mut world, &mut state);

    let plan = world.view.prepare_detach(ws(A, 1)).unwrap();
    let detached = world.view.apply_detach(plan).unwrap();
    let invalidation = *detached
        .invalidations
        .iter()
        .find(|invalidation| invalidation.pane == pane(A, 1))
        .unwrap();
    assert_eq!(invalidation.reason, InvalidationReason::Detached);
    let decision = state
        .reduce(&PresentationFacts::invalidated(invalidation))
        .unwrap();
    assert_eq!(
        decision.presentation,
        PanePresentation::Unavailable {
            reason: InvalidationReason::Detached
        }
    );
    assert_content_free(&decision);
    assert!(decision.clear_content);
    assert!(decision.invalidate_native_context);
    assert!(!decision.presentation.content_eligible());

    // Observing the detached Pane again finds no attachment.
    let missing = present(&world, &mut state, &StartupIntents::new(), 1, Some(session));
    assert_eq!(
        missing.presentation,
        PanePresentation::Unavailable {
            reason: InvalidationReason::TargetDeleted
        }
    );
    assert_content_free(&missing);
    assert!(missing.clear_content);
    assert!(missing.invalidate_native_context);

    // A state is bound to its Pane.
    let other = *detached
        .invalidations
        .iter()
        .find(|invalidation| invalidation.pane == pane(A, 2))
        .unwrap();
    assert_eq!(
        state.reduce(&PresentationFacts::invalidated(other)),
        Err(ViewError::WrongMembership)
    );

    // A restored attachment matched by a durable marker.
    let mut world = World::new();
    world.set(1, running(1, Some(1), 1));
    let retained = world.view.persisted().unwrap();
    let mut filter = RestorationFilter::new();
    filter.suppress_attachment(world.view.workspace(ws(A, 1)).unwrap().attachment());
    let (_, update) = ClientViewState::restore(Some(&retained), &filter).unwrap();
    let suppressed = *update
        .invalidations
        .iter()
        .find(|invalidation| invalidation.pane == pane(A, 1))
        .unwrap();
    let mut state = PresentationState::new();
    let decision = state
        .reduce(&PresentationFacts::invalidated(suppressed))
        .unwrap();
    assert_eq!(
        decision.presentation,
        PanePresentation::Unavailable {
            reason: InvalidationReason::Suppressed
        }
    );
    assert_content_free(&decision);
    assert!(decision.clear_content);
    assert!(decision.invalidate_native_context);

    // Self-unpair removes the pairing's attachments.
    let mut state = PresentationState::new();
    live(&mut world, &mut state);
    let removed = world
        .view
        .remove_pairing(world.org.server(), pairing(A, 1))
        .unwrap();
    let unpaired = *removed
        .invalidations
        .iter()
        .find(|invalidation| invalidation.pane == pane(A, 1))
        .unwrap();
    let decision = state
        .reduce(&PresentationFacts::invalidated(unpaired))
        .unwrap();
    assert_eq!(
        decision.presentation,
        PanePresentation::Unavailable {
            reason: InvalidationReason::PairingRemoved
        }
    );
    assert_content_free(&decision);
    assert!(decision.clear_content);
    assert!(decision.invalidate_native_context);
}

#[test]
fn same_session_interruption_keeps_its_frame_and_revokes_only_interaction_context() {
    let mut world = World::new();
    let mut state = PresentationState::new();
    let session = live(&mut world, &mut state);

    // Going offline revokes IME, prepared copies, input and native effects.
    // The frame, its read-only accessibility facts, the selection and Find
    // results stay.
    world.facts = offline(A);
    let offline = present(&world, &mut state, &StartupIntents::new(), 1, Some(session));
    assert_eq!(offline.content, ContentDisposition::KeepFrozen { session });
    assert_eq!(offline.eligibility, OFFLINE_FROZEN);
    assert!(offline.invalidate_native_context);
    assert!(!offline.clear_content);

    // Reconnecting withdraws accessibility exposure through eligibility only.
    world.facts = synchronizing(A);
    let reconciling = present(&world, &mut state, &StartupIntents::new(), 1, Some(session));
    assert_eq!(
        reconciling.content,
        ContentDisposition::KeepFrozen { session }
    );
    assert_eq!(reconciling.eligibility, RECONCILING_FROZEN);
    assert!(!reconciling.invalidate_native_context);
    assert!(!reconciling.clear_content);

    // Reconciliation confirms the same Session under this Client's control.
    world.facts = ready(A);
    let resumed = present(&world, &mut state, &StartupIntents::new(), 1, Some(session));
    assert_eq!(resumed.presentation, PanePresentation::Live);
    assert_eq!(resumed.content, ContentDisposition::Allow { session });
    assert_eq!(resumed.eligibility, LIVE);
    assert!(!resumed.invalidate_native_context);
    assert!(!resumed.clear_content);

    // Straight from Live to Reconciling also keeps the frame.
    world.facts = synchronizing(A);
    let direct = present(&world, &mut state, &StartupIntents::new(), 1, Some(session));
    assert_eq!(direct.content, ContentDisposition::KeepFrozen { session });
    assert!(direct.invalidate_native_context);
    assert!(!direct.clear_content);
}

#[test]
fn unclaimed_pane_is_distinct_from_takeover_and_shows_its_acquisition() {
    let mut world = World::new();
    world.set(1, running(1, None, 3));
    let mut state = PresentationState::new();
    let mut intents = StartupIntents::new();
    let unclaimed = present(&world, &mut state, &intents, 1, None);
    assert_eq!(
        unclaimed.presentation,
        PanePresentation::Unclaimed { acquiring: false }
    );
    assert_eq!(unclaimed.controller, None);
    assert_content_free(&unclaimed);
    assert!(!unclaimed.presentation.content_eligible());

    // Its action is the Pane's own selection, a compare-acquire, and the
    // decision shows the acquisition while it is in flight.
    let acquire = world.begin(&mut intents, target(1, 1), SELECT).unwrap();
    assert!(matches!(
        acquire.effects[..],
        [StartupEffect::AcquireControl { .. }]
    ));
    let acquiring = present(&world, &mut state, &intents, 1, None);
    assert_eq!(
        acquiring.presentation,
        PanePresentation::Unclaimed { acquiring: true }
    );
    assert_content_free(&acquiring);

    // An acquisition requested on an older connection is not shown.
    let reconnected = ReplicaFacts {
        connection: Some(ConnectionBinding::new(
            client(A, 1),
            ConnectionGeneration::new(2),
        )),
        ..ready(A)
    };
    world.facts = reconnected;
    assert_eq!(
        present(&world, &mut state, &intents, 1, None).presentation,
        PanePresentation::Unclaimed { acquiring: false }
    );
    world.facts = ready(A);

    // A release and reacquire race leaves the Pane unclaimed at a later
    // control generation: the old acquisition is no longer shown.
    world.set(1, running(1, None, 5));
    assert_eq!(
        present(&world, &mut state, &intents, 1, None).presentation,
        PanePresentation::Unclaimed { acquiring: false }
    );

    // Another Client wins the race: the Takeover Screen names it.
    let mut world = World::new();
    world.set(1, running(1, None, 3));
    let mut intents = StartupIntents::new();
    world.begin(&mut intents, target(1, 1), SELECT).unwrap();
    world.set(1, running(1, Some(2), 4));
    let lost = present(&world, &mut state, &intents, 1, None);
    assert_eq!(lost.presentation, PanePresentation::Takeover);
    assert_eq!(lost.controller, Some(client(A, 2)));
    assert_content_free(&lost);

    // This Client wins it: the Pane is live.
    let mut world = World::new();
    world.set(1, running(1, None, 3));
    let mut intents = StartupIntents::new();
    let mut state = PresentationState::new();
    world.begin(&mut intents, target(1, 1), SELECT).unwrap();
    world.set(1, running(1, Some(1), 4));
    assert_eq!(
        present(&world, &mut state, &intents, 1, None).presentation,
        PanePresentation::Live
    );
}

#[test]
fn unclaimed_pane_without_a_session_keeps_its_phase_screen() {
    let starting = SessionPhase::Starting {
        generation: SessionGeneration::new(1),
    };
    let failed = SessionPhase::Failed {
        class: FailureClass::LaunchUnavailable,
    };
    let restored = SessionPhase::Ended {
        reason: EndReason::ServerRestart,
    };
    let end_all = SessionPhase::Ended {
        reason: EndReason::EndAllSessions,
    };
    // Unclaimed: a Session that runs or starts is withheld behind the
    // Unclaimed screen. Without one, the phase screen's own Start Session or
    // Retry compare-acquires, so it is kept: restored Panes are unclaimed.
    let unclaimed = [
        (
            SessionPhase::Running {
                generation: SessionGeneration::new(1),
            },
            PanePresentation::Unclaimed { acquiring: false },
        ),
        (starting, PanePresentation::Unclaimed { acquiring: false }),
        (SessionPhase::NotStarted, PanePresentation::NotStarted),
        (
            restored,
            PanePresentation::Ended {
                reason: EndReason::ServerRestart,
            },
        ),
        (
            end_all,
            PanePresentation::Ended {
                reason: EndReason::EndAllSessions,
            },
        ),
        (
            failed,
            PanePresentation::Failed {
                class: FailureClass::LaunchUnavailable,
            },
        ),
        (
            SessionPhase::DirectoryUnavailable,
            PanePresentation::DirectoryUnavailable,
        ),
    ];
    for (phase, expected) in unclaimed {
        let mut world = World::new();
        world.set(
            1,
            Runtime {
                session: 1,
                phase,
                control_generation: 2,
                control: ControlRecord::Unclaimed,
            },
        );
        let decision = present(
            &world,
            &mut PresentationState::new(),
            &StartupIntents::new(),
            1,
            None,
        );
        assert_eq!(decision.presentation, expected, "{phase:?}");
        assert_eq!(decision.controller, None);
        assert_content_free(&decision);
    }

    // Held by this Client: the phase screen, Starting progress or Live.
    // Held by another Client: the Takeover Screen naming it, in every phase.
    let phases = [
        SessionPhase::Running {
            generation: SessionGeneration::new(1),
        },
        starting,
        SessionPhase::NotStarted,
        restored,
        end_all,
        failed,
        SessionPhase::DirectoryUnavailable,
    ];
    for phase in phases {
        let mut world = World::new();
        let runtime = |client_local| Runtime {
            session: 1,
            phase,
            control_generation: 2,
            control: held(client_local, 2),
        };
        world.set(1, runtime(1));
        let own = present(
            &world,
            &mut PresentationState::new(),
            &StartupIntents::new(),
            1,
            None,
        );
        let expected = match phase {
            SessionPhase::Running { .. } => PanePresentation::Live,
            SessionPhase::Starting { .. } => PanePresentation::Starting,
            SessionPhase::NotStarted => PanePresentation::NotStarted,
            SessionPhase::Ended { reason } => PanePresentation::Ended { reason },
            SessionPhase::Failed { class } => PanePresentation::Failed { class },
            SessionPhase::DirectoryUnavailable => PanePresentation::DirectoryUnavailable,
        };
        assert_eq!(own.presentation, expected, "{phase:?}");

        world.set(1, runtime(2));
        let other = present(
            &world,
            &mut PresentationState::new(),
            &StartupIntents::new(),
            1,
            None,
        );
        assert_eq!(other.presentation, PanePresentation::Takeover, "{phase:?}");
        assert_eq!(other.controller, Some(client(A, 2)));
        assert_content_free(&other);
    }

    // Start Session on an unclaimed ended Pane requests one control-checked
    // use, which client-core sends as compare-acquire plus startup.
    let mut world = World::new();
    world.set(
        1,
        Runtime {
            session: 1,
            phase: end_all,
            control_generation: 2,
            control: ControlRecord::Unclaimed,
        },
    );
    let mut intents = StartupIntents::new();
    let start = world.begin(&mut intents, target(1, 1), START).unwrap();
    let StartupEffect::RequestUse { context, .. } = start.effects[0] else {
        panic!("Start Session requests a use");
    };
    assert_eq!(context.control(), ControlRecord::Unclaimed);
}
