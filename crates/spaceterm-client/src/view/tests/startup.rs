//! Explicit-use choreography against real views and validated core snapshots.
//!
//! Every scenario records the effect sequence and checks the target Pane, the
//! exact server intent, the measured grid and the fence, not only a count.

use std::time::{Duration, Instant};

use spaceterm_core::authority::{
    ConnectionBinding, ConnectionGeneration, ContextGeneration, ControlGeneration, ControlGrant,
    EffectFence, SessionGeneration, SessionKey,
};
use spaceterm_core::control::StartupSource;
use spaceterm_core::geometry::{CellPixels, GridSize, SUPPORTED_GRID_LIMITS};
use spaceterm_core::identity::{OperationStreamId, OperationStreamKey};
use spaceterm_core::operations::{OperationId, OperationSequence};
use spaceterm_core::organization::{
    ControlRecord, EndReason, FailureClass, OrganizationSnapshot, SessionPhase, SplitAxis,
    SplitEdge,
};

use super::{
    A, Org, Runtime, attach, chain, client, observe, offline, pairing, pane, ready, reconcile,
    restored, retained_workspace, server, synchronizing, tab, ws,
};
use crate::view::{
    ArrangementEdit, AuthorizedUse, ClientViewState, ExplicitUseSource, IntentCancelReason,
    LayoutRevision, MAX_TAB_PANES, MAX_VIEW_PANES, Measurement, PanePresentation,
    PresentationFacts, PresentationState, ReplicaFacts, SelectionSource, SelectionTarget,
    StartupEffect, StartupIntents, StartupReduction, StartupState, UseContext, UseFacts, UseKind,
    UseTicket, ViewCounter, ViewError,
};

pub(super) const POINTER: SelectionSource = SelectionSource::Pointer;
pub(super) const SELECT: ExplicitUseSource = ExplicitUseSource::PaneSelection(POINTER);
pub(super) const START: ExplicitUseSource = ExplicitUseSource::StartSession(POINTER);
pub(super) const RETRY: ExplicitUseSource =
    ExplicitUseSource::Retry(SelectionSource::AccessibilityPress);
pub(super) const OPEN: ExplicitUseSource = ExplicitUseSource::TabOpening(SelectionSource::Command);

pub(super) fn ticket(value: u64) -> UseTicket {
    UseTicket::new(value).unwrap()
}

pub(super) fn grid(rows: u32, columns: u32) -> GridSize {
    GridSize::new(
        rows,
        columns,
        CellPixels::new(8, 16).unwrap(),
        SUPPORTED_GRID_LIMITS,
    )
    .unwrap()
}

/// A server intent of this Client's operation stream.
pub(super) fn intent(sequence: u64) -> OperationId {
    let stream = OperationStreamId::new(
        OperationStreamKey::new(server(A), 1).unwrap(),
        pairing(A, 1),
        client(A, 1),
    )
    .unwrap();
    OperationId::new(stream, OperationSequence::new(sequence).unwrap())
}

pub(super) fn held(client_local: u64, generation: u64) -> ControlRecord {
    ControlRecord::Held {
        client: client(A, client_local),
        generation: ControlGeneration::new(generation),
    }
}

/// A restored Pane this Client controls.
pub(super) fn ended_held(session: u64, reason: EndReason) -> Runtime {
    Runtime {
        session,
        phase: SessionPhase::Ended { reason },
        control_generation: 1,
        control: held(1, 1),
    }
}

pub(super) fn running(session: u64, client_local: Option<u64>, control_generation: u64) -> Runtime {
    Runtime {
        session,
        phase: SessionPhase::Running {
            generation: SessionGeneration::new(session),
        },
        control_generation,
        control: client_local.map_or(ControlRecord::Unclaimed, |local| {
            held(local, control_generation)
        }),
    }
}

pub(super) fn target(tab_local: u64, pane_local: u64) -> SelectionTarget {
    SelectionTarget {
        workspace: ws(A, 1),
        tab: tab(A, tab_local),
        pane: pane(A, pane_local),
    }
}

/// Workspace 1: Tab 1 holds Panes 1 and 2, Tab 2 holds Panes 3 and 4. Tab 1
/// is active with Pane 1 focused.
pub(super) struct World {
    pub(super) org: Org,
    pub(super) view: ClientViewState,
    pub(super) facts: ReplicaFacts,
    pub(super) context: ContextGeneration,
}

impl World {
    pub(super) fn new() -> Self {
        let mut org = Org::new(A);
        org.add_workspace(1, 1, 1)
            .split(1, 2, 1, SplitAxis::Horizontal, SplitEdge::After)
            .add_tab(1, 2, 3, None)
            .split(2, 4, 3, SplitAxis::Vertical, SplitEdge::After);
        let mut view = ClientViewState::new();
        attach(&mut view, &org, ready(A), 1).unwrap();
        let mut world = Self {
            org,
            view,
            facts: ready(A),
            context: ContextGeneration::new(1),
        };
        world.activate(1);
        world.focus(1, 1);
        world
    }

    pub(super) fn with<R>(&self, run: impl FnOnce(UseFacts<'_>) -> R) -> R {
        let snapshot = self.org.snapshot();
        let replica = observe(&snapshot, &[], self.facts);
        run(UseFacts {
            view: &self.view,
            replica: &replica,
            context: self.context,
        })
    }

    pub(super) fn set(&mut self, pane_local: u64, runtime: Runtime) {
        self.org.set_runtime(pane_local, runtime);
    }

    pub(super) fn focus(&mut self, tab_local: u64, pane_local: u64) {
        self.view
            .arrange(
                ws(A, 1),
                tab(A, tab_local),
                ArrangementEdit::Focus(pane(A, pane_local)),
            )
            .unwrap();
    }

    pub(super) fn activate(&mut self, tab_local: u64) {
        self.view.activate_tab(ws(A, 1), tab(A, tab_local)).unwrap();
    }

    pub(super) fn layout(&self, tab_local: u64) -> LayoutRevision {
        self.view
            .workspace(ws(A, 1))
            .unwrap()
            .arrangement(tab(A, tab_local))
            .unwrap()
            .layout_revision()
    }

    pub(super) fn context_of(&self, target: SelectionTarget) -> UseContext {
        self.with(|facts| UseContext::observe(facts.view, facts.replica, target, facts.context))
            .unwrap()
    }

    /// The exact fence client-core reports for a Pane under the current
    /// Session, this connection and context, and the given grant generation.
    pub(super) fn fence(&self, pane_local: u64, grant: u64) -> EffectFence {
        let snapshot = self.org.snapshot();
        let record = snapshot
            .runtime()
            .iter()
            .find(|record| record.pane == pane(A, pane_local))
            .unwrap();
        EffectFence::new(
            SessionKey {
                epoch: snapshot.epoch(),
                pane: pane(A, pane_local),
                generation: record.session_generation,
            },
            ControlGrant::new(
                pane(A, pane_local),
                client(A, 1),
                ControlGeneration::new(grant),
            )
            .unwrap(),
            self.facts.connection.unwrap(),
            self.context,
            None,
        )
        .unwrap()
    }

    pub(super) fn begin(
        &self,
        intents: &mut StartupIntents,
        target: SelectionTarget,
        source: ExplicitUseSource,
    ) -> Result<StartupReduction, ViewError> {
        self.with(|facts| intents.begin(facts, target, source))
    }

    pub(super) fn authorize(
        &self,
        intents: &mut StartupIntents,
        ticket: UseTicket,
        intent: OperationId,
        fence: EffectFence,
    ) -> Result<StartupReduction, ViewError> {
        self.with(|facts| {
            intents.authorized(
                facts,
                AuthorizedUse {
                    ticket,
                    intent,
                    fence,
                },
            )
        })
    }

    pub(super) fn measure(
        &self,
        intents: &mut StartupIntents,
        ticket: UseTicket,
        layout: LayoutRevision,
        grid: GridSize,
    ) -> Result<StartupReduction, ViewError> {
        self.with(|facts| {
            intents.measured(
                facts,
                Measurement {
                    ticket,
                    context: self.context,
                    layout,
                    grid,
                },
            )
        })
    }

    pub(super) fn revalidate(&self, intents: &mut StartupIntents) -> StartupReduction {
        self.with(|facts| intents.revalidate(facts))
    }

    /// Begins and authorizes a startup of Pane 1, held by this Client, with
    /// ticket 1 and intent 1.
    pub(super) fn authorized_use(&mut self, intents: &mut StartupIntents) {
        self.set(1, ended_held(1, EndReason::ServerRestart));
        self.begin(intents, target(1, 1), SELECT).unwrap();
        let reduction = self
            .authorize(intents, ticket(1), intent(1), self.fence(1, 1))
            .unwrap();
        assert_eq!(reduction.effects, Vec::new());
        assert_eq!(reduction.state, StartupState::AwaitingMeasurement);
    }
}

fn request_measurement(
    world: &World,
    value: u64,
    target: SelectionTarget,
    layout: LayoutRevision,
) -> StartupEffect {
    StartupEffect::RequestMeasurement {
        ticket: ticket(value),
        target,
        context: world.context,
        layout,
    }
}

fn completion(
    value: u64,
    intent_sequence: u64,
    grid: GridSize,
    fence: EffectFence,
) -> StartupEffect {
    StartupEffect::CompleteUse {
        ticket: ticket(value),
        intent: intent(intent_sequence),
        grid,
        fence,
    }
}

fn cancel_use(value: u64, intent_sequence: u64) -> StartupEffect {
    StartupEffect::CancelUse {
        ticket: ticket(value),
        intent: intent(intent_sequence),
    }
}

fn starts_or_acquires(effect: &StartupEffect) -> bool {
    matches!(
        effect,
        StartupEffect::RequestUse { .. }
            | StartupEffect::AcquireControl { .. }
            | StartupEffect::CompleteUse { .. }
    )
}

#[test]
fn measurement_without_explicit_use_starts_nothing() {
    let mut world = World::new();
    world.set(1, ended_held(1, EndReason::ServerRestart));
    let mut intents = StartupIntents::new();

    assert_eq!(
        world.measure(&mut intents, ticket(1), world.layout(1), grid(24, 80)),
        Err(ViewError::StaleIntent)
    );
    let reduction = world.revalidate(&mut intents);
    assert_eq!(reduction.effects, Vec::new());
    assert_eq!(reduction.state, StartupState::Idle);
    assert_eq!(intents.use_for(pane(A, 1)), None);

    // A measurement reported again for a finished use completes nothing new.
    let fence = world.fence(1, 1);
    world.begin(&mut intents, target(1, 1), SELECT).unwrap();
    world
        .authorize(&mut intents, ticket(1), intent(1), fence)
        .unwrap();
    let done = world
        .measure(&mut intents, ticket(1), world.layout(1), grid(24, 80))
        .unwrap();
    assert_eq!(done.effects, vec![completion(1, 1, grid(24, 80), fence)]);
    let again = world
        .measure(&mut intents, ticket(1), world.layout(1), grid(30, 90))
        .unwrap();
    assert_eq!(again.effects, Vec::new());
}

#[test]
fn restored_activation_and_window_focus_start_nothing() {
    let mut org = Org::new(A);
    org.add_workspace(1, 1, 1)
        .split(1, 2, 1, SplitAxis::Horizontal, SplitEdge::After)
        .add_tab(1, 2, 3, None)
        .split(2, 4, 3, SplitAxis::Horizontal, SplitEdge::After)
        .set_runtime(1, ended_held(1, EndReason::ServerRestart))
        .set_runtime(3, ended_held(1, EndReason::ServerRestart));
    let mut view = restored(
        3,
        1,
        vec![retained_workspace(
            A,
            1,
            1,
            vec![(1, chain(A, 1, 2)), (2, chain(A, 3, 2))],
        )],
    );
    let mut intents = StartupIntents::new();
    let snapshot = org.snapshot();

    for facts in [offline(A), synchronizing(A), ready(A)] {
        let replica = observe(&snapshot, &[], facts);
        let update = view.reconcile(&replica).unwrap();
        assert!(update.invalidations.is_empty());
        let reduction = intents.revalidate(UseFacts {
            view: &view,
            replica: &replica,
            context: ContextGeneration::new(1),
        });
        assert_eq!(reduction.effects, Vec::new());
        assert_eq!(reduction.state, StartupState::Idle);
    }

    // Restored navigation and repeated window focus are not explicit use.
    view.activate_tab(ws(A, 1), tab(A, 2)).unwrap();
    view.activate_workspace(ws(A, 1)).unwrap();
    let replica = observe(&snapshot, &[], ready(A));
    for context in [1, 1, 2] {
        let reduction = intents.revalidate(UseFacts {
            view: &view,
            replica: &replica,
            context: ContextGeneration::new(context),
        });
        assert_eq!(reduction.effects, Vec::new());
    }
    assert_eq!(intents.state(), StartupState::Idle);
    assert_eq!(intents.use_for(pane(A, 3)), None);
}

#[test]
fn explicit_use_without_measurement_awaits_dimensions() {
    let mut world = World::new();
    let mut intents = StartupIntents::new();
    let selected = target(1, 1);
    let context = world.context_of(selected);
    assert_eq!(context.control(), ControlRecord::Unclaimed);
    assert_eq!(context.phase(), SessionPhase::NotStarted);

    let reduction = world.begin(&mut intents, selected, SELECT).unwrap();
    assert_eq!(
        reduction.effects,
        vec![
            StartupEffect::RequestUse {
                ticket: ticket(1),
                target: selected,
                source: SELECT,
                kind: StartupSource::ExplicitSelection,
                context,
            },
            request_measurement(&world, 1, selected, world.layout(1)),
        ]
    );
    assert_eq!(
        reduction.state,
        StartupState::AwaitingAuthorization { measured: false }
    );

    // The compare-acquire grant arrives before any dimensions.
    let authorized = world
        .authorize(&mut intents, ticket(1), intent(1), world.fence(1, 1))
        .unwrap();
    assert_eq!(authorized.effects, Vec::new());
    assert_eq!(authorized.state, StartupState::AwaitingMeasurement);
    let pending = intents.use_for(pane(A, 1)).unwrap();
    assert_eq!(pending.ticket, ticket(1));
    assert_eq!(pending.kind, UseKind::Startup);
    assert_eq!(pending.context, context);

    world.set(
        1,
        Runtime {
            control_generation: 1,
            control: held(1, 1),
            ..Runtime::default()
        },
    );
    let reconciled = world.revalidate(&mut intents);
    assert_eq!(reconciled.effects, Vec::new());
    assert_eq!(reconciled.state, StartupState::AwaitingMeasurement);
}

#[test]
fn authorization_and_measurement_may_arrive_in_either_order() {
    for authorization_first in [true, false] {
        let mut world = World::new();
        world.set(1, ended_held(4, EndReason::EndAllSessions));
        let mut intents = StartupIntents::new();
        let fence = world.fence(1, 1);
        world.begin(&mut intents, target(1, 1), START).unwrap();

        let (first, second) = if authorization_first {
            let first = world
                .authorize(&mut intents, ticket(1), intent(7), fence)
                .unwrap();
            assert_eq!(first.state, StartupState::AwaitingMeasurement);
            let second = world
                .measure(&mut intents, ticket(1), world.layout(1), grid(40, 120))
                .unwrap();
            (first, second)
        } else {
            let first = world
                .measure(&mut intents, ticket(1), world.layout(1), grid(40, 120))
                .unwrap();
            assert_eq!(
                first.state,
                StartupState::AwaitingAuthorization { measured: true }
            );
            let second = world
                .authorize(&mut intents, ticket(1), intent(7), fence)
                .unwrap();
            (first, second)
        };
        assert_eq!(first.effects, Vec::new());
        assert_eq!(second.effects, vec![completion(1, 7, grid(40, 120), fence)]);
        assert_eq!(second.state, StartupState::Submitted);
        assert_eq!(fence.session().pane, pane(A, 1));
        assert_eq!(fence.session().generation, SessionGeneration::new(4));
    }
}

#[test]
fn first_current_measurement_completes_exact_intent_once() {
    let mut world = World::new();
    world.set(1, ended_held(1, EndReason::ServerRestart));
    let mut intents = StartupIntents::new();
    let fence = world.fence(1, 1);
    world.begin(&mut intents, target(1, 1), SELECT).unwrap();

    // An intent bound to another Pane or a content revision is rejected.
    let wrong_pane = world.fence(2, 1);
    assert_eq!(
        world.authorize(&mut intents, ticket(1), intent(3), wrong_pane),
        Err(ViewError::WrongMembership)
    );
    assert_eq!(
        intents.state(),
        StartupState::AwaitingAuthorization { measured: false }
    );
    world
        .authorize(&mut intents, ticket(1), intent(3), fence)
        .unwrap();

    let reduction = world
        .measure(&mut intents, ticket(1), world.layout(1), grid(24, 80))
        .unwrap();
    assert_eq!(
        reduction.effects,
        vec![completion(1, 3, grid(24, 80), fence)]
    );
    assert_eq!(reduction.state, StartupState::Submitted);
    assert_eq!(
        intents.use_for(pane(A, 1)).unwrap().state,
        StartupState::Submitted
    );

    // Repeated measurement, authorization, repaint and re-selection while the
    // completion is in flight emit nothing.
    let repeats = [
        world
            .measure(&mut intents, ticket(1), world.layout(1), grid(24, 80))
            .unwrap(),
        world
            .measure(&mut intents, ticket(1), world.layout(1), grid(50, 100))
            .unwrap(),
        world
            .authorize(&mut intents, ticket(1), intent(3), fence)
            .unwrap(),
        world.revalidate(&mut intents),
        world.begin(&mut intents, target(1, 1), SELECT).unwrap(),
    ];
    for repeat in repeats {
        assert_eq!(repeat.effects, Vec::new());
        assert_eq!(repeat.state, StartupState::Submitted);
    }

    // The server reserves the new Session generation: the hand-off is done.
    world.set(
        1,
        Runtime {
            session: 2,
            phase: SessionPhase::Starting {
                generation: SessionGeneration::new(2),
            },
            control_generation: 1,
            control: held(1, 1),
        },
    );
    let reserved = world.revalidate(&mut intents);
    assert_eq!(reserved.effects, Vec::new());
    assert_eq!(reserved.state, StartupState::Idle);
    assert_eq!(intents.use_for(pane(A, 1)), None);
}

#[test]
fn unchanged_grid_is_reported_for_a_new_measurement_ticket() {
    let mut world = World::new();
    world.set(1, ended_held(1, EndReason::ServerRestart));
    let mut intents = StartupIntents::new();
    let first_fence = world.fence(1, 1);
    world.begin(&mut intents, target(1, 1), SELECT).unwrap();
    world
        .authorize(&mut intents, ticket(1), intent(1), first_fence)
        .unwrap();
    let first = world
        .measure(&mut intents, ticket(1), world.layout(1), grid(24, 80))
        .unwrap();
    assert_eq!(
        first.effects,
        vec![completion(1, 1, grid(24, 80), first_fence)]
    );

    // The Session runs, then End All Sessions ends it.
    world.set(1, running(2, Some(1), 1));
    world.revalidate(&mut intents);
    world.set(1, ended_held(2, EndReason::EndAllSessions));
    assert_eq!(world.revalidate(&mut intents).effects, Vec::new());

    let second = world.begin(&mut intents, target(1, 1), START).unwrap();
    assert_eq!(
        second.effects[1],
        request_measurement(&world, 2, target(1, 1), world.layout(1))
    );
    let second_fence = world.fence(1, 1);
    assert_ne!(second_fence, first_fence);
    world
        .authorize(&mut intents, ticket(2), intent(2), second_fence)
        .unwrap();
    let same_grid = world
        .measure(&mut intents, ticket(2), world.layout(1), grid(24, 80))
        .unwrap();
    assert_eq!(
        same_grid.effects,
        vec![completion(2, 2, grid(24, 80), second_fence)]
    );
}

#[test]
fn stale_layout_measurement_requests_current_measurement() {
    let mut world = World::new();
    world.set(1, ended_held(1, EndReason::ServerRestart));
    let mut intents = StartupIntents::new();
    let fence = world.fence(1, 1);
    let old_layout = world.layout(1);
    world.begin(&mut intents, target(1, 1), SELECT).unwrap();

    world
        .view
        .arrange(ws(A, 1), tab(A, 1), ArrangementEdit::ToggleZoom)
        .unwrap();
    let new_layout = world.layout(1);
    assert_ne!(new_layout, old_layout);

    let stale = world
        .measure(&mut intents, ticket(1), old_layout, grid(24, 80))
        .unwrap();
    assert_eq!(
        stale.effects,
        vec![request_measurement(&world, 1, target(1, 1), new_layout)]
    );
    assert_eq!(
        stale.state,
        StartupState::AwaitingAuthorization { measured: false }
    );
    let authorized = world
        .authorize(&mut intents, ticket(1), intent(1), fence)
        .unwrap();
    assert_eq!(authorized.effects, Vec::new());

    // A late measurement of the old layout still cannot complete.
    let late = world
        .measure(&mut intents, ticket(1), old_layout, grid(24, 80))
        .unwrap();
    assert_eq!(late.effects, Vec::new());
    let current = world
        .measure(&mut intents, ticket(1), new_layout, grid(48, 160))
        .unwrap();
    assert_eq!(
        current.effects,
        vec![completion(1, 1, grid(48, 160), fence)]
    );
}

#[test]
fn tab_open_targets_only_remembered_focused_pane() {
    let mut world = World::new();
    world.focus(2, 4);
    world.activate(1);
    let mut intents = StartupIntents::new();

    world.activate(2);
    let opened = SelectionTarget::focused(&world.view, ws(A, 1)).unwrap();
    assert_eq!(opened, target(2, 4));

    // Neither the other Pane of the opened Tab nor a Pane of the Tab just left
    // can be used.
    assert_eq!(
        world.begin(&mut intents, target(2, 3), OPEN),
        Err(ViewError::StaleIntent)
    );
    assert_eq!(
        world.begin(&mut intents, target(1, 1), SELECT),
        Err(ViewError::StaleIntent)
    );
    assert_eq!(intents.state(), StartupState::Idle);

    let reduction = world.begin(&mut intents, opened, OPEN).unwrap();
    assert_eq!(
        reduction.effects,
        vec![
            StartupEffect::RequestUse {
                ticket: ticket(1),
                target: target(2, 4),
                source: OPEN,
                kind: StartupSource::TabOpening { tab: tab(A, 2) },
                context: world.context_of(target(2, 4)),
            },
            request_measurement(&world, 1, target(2, 4), world.layout(2)),
        ]
    );
    assert_eq!(intents.use_for(pane(A, 3)), None);
}

#[test]
fn selection_of_other_clients_pane_never_requests_takeover() {
    let mut world = World::new();
    world.set(1, running(1, Some(2), 3));
    world.set(
        2,
        Runtime {
            control_generation: 5,
            control: held(2, 5),
            ..ended_held(1, EndReason::EndAllSessions)
        },
    );
    let mut intents = StartupIntents::new();

    for source in [SELECT, START, RETRY, OPEN] {
        let reduction = world.begin(&mut intents, target(1, 1), source).unwrap();
        assert_eq!(reduction.effects, Vec::new());
        assert_eq!(reduction.state, StartupState::Idle);
    }
    world.focus(1, 2);
    let ended = world.begin(&mut intents, target(1, 2), START).unwrap();
    assert_eq!(ended.effects, Vec::new());

    // A selection of the other Client's Pane still supersedes this Client's
    // older unsubmitted use, and requests nothing for itself.
    world.set(2, Runtime::default());
    let pending = world.begin(&mut intents, target(1, 2), SELECT).unwrap();
    assert!(pending.effects.iter().any(starts_or_acquires));
    world.focus(1, 1);
    let superseding = world.begin(&mut intents, target(1, 1), SELECT).unwrap();
    assert_eq!(superseding.effects, Vec::new());
    assert_eq!(superseding.canceled, Some(IntentCancelReason::Superseded));
    assert_eq!(superseding.state, StartupState::Idle);
}

#[test]
fn lost_compare_acquire_race_never_completes_startup() {
    // The grant reaches client-core before the replica shows another winner.
    let mut world = World::new();
    let mut intents = StartupIntents::new();
    world.begin(&mut intents, target(1, 1), SELECT).unwrap();
    world
        .measure(&mut intents, ticket(1), world.layout(1), grid(24, 80))
        .unwrap();
    let waiting = world
        .authorize(&mut intents, ticket(1), intent(1), world.fence(1, 1))
        .unwrap();
    assert_eq!(waiting.effects, Vec::new());
    assert_eq!(waiting.state, StartupState::ReadyToComplete);

    world.set(
        1,
        Runtime {
            control_generation: 1,
            control: held(2, 1),
            ..Runtime::default()
        },
    );
    let lost = world.revalidate(&mut intents);
    assert_eq!(lost.effects, vec![cancel_use(1, 1)]);
    assert_eq!(lost.canceled, Some(IntentCancelReason::ControlLost));
    assert_eq!(lost.state, StartupState::Idle);

    // The replica shows the other winner before any intent is reported.
    let mut world = World::new();
    let mut intents = StartupIntents::new();
    world.begin(&mut intents, target(1, 1), SELECT).unwrap();
    world.set(
        1,
        Runtime {
            control_generation: 1,
            control: held(2, 1),
            ..Runtime::default()
        },
    );
    let lost = world.revalidate(&mut intents);
    assert_eq!(lost.effects, Vec::new());
    assert_eq!(lost.canceled, Some(IntentCancelReason::ControlLost));
    let late = world
        .authorize(&mut intents, ticket(1), intent(1), world.fence(1, 1))
        .unwrap();
    assert_eq!(late.effects, vec![cancel_use(1, 1)]);
    let measured = world
        .measure(&mut intents, ticket(1), world.layout(1), grid(24, 80))
        .unwrap();
    assert!(!measured.effects.iter().any(starts_or_acquires));

    // A release and reacquire race is a lost grant, not this Client's.
    let mut world = World::new();
    let mut intents = StartupIntents::new();
    world.begin(&mut intents, target(1, 1), SELECT).unwrap();
    world.set(
        1,
        Runtime {
            control_generation: 2,
            ..Runtime::default()
        },
    );
    let raced = world.revalidate(&mut intents);
    assert_eq!(raced.canceled, Some(IntentCancelReason::ControlLost));
}

#[test]
fn control_loss_detach_tab_switch_and_disconnect_cancel_unsubmitted_intent() {
    type Event = fn(&mut World);
    let events: [(Event, IntentCancelReason); 4] = [
        (
            |world| {
                world.set(
                    1,
                    Runtime {
                        control_generation: 2,
                        control: held(2, 2),
                        ..ended_held(1, EndReason::ServerRestart)
                    },
                )
            },
            IntentCancelReason::ControlLost,
        ),
        (
            |world| {
                let plan = world.view.prepare_detach(ws(A, 1)).unwrap();
                world.view.apply_detach(plan).unwrap();
            },
            IntentCancelReason::Detached,
        ),
        (|world| world.activate(2), IntentCancelReason::LeftSelection),
        (
            |world| world.facts = offline(A),
            IntentCancelReason::ConnectionLost,
        ),
    ];
    for (event, reason) in events {
        let mut world = World::new();
        let mut intents = StartupIntents::new();
        world.authorized_use(&mut intents);
        let layout = world.layout(1);
        event(&mut world);

        let canceled = world.revalidate(&mut intents);
        assert_eq!(canceled.effects, vec![cancel_use(1, 1)]);
        assert_eq!(canceled.canceled, Some(reason));
        assert_eq!(canceled.state, StartupState::Idle);
        let measured = world
            .measure(&mut intents, ticket(1), layout, grid(24, 80))
            .unwrap();
        assert_eq!(measured.effects, Vec::new());
        assert_eq!(intents.use_for(pane(A, 1)), None);
    }

    // Detach and self-unpair can also cancel directly.
    let mut world = World::new();
    let mut intents = StartupIntents::new();
    world.authorized_use(&mut intents);
    let canceled = intents.cancel(IntentCancelReason::PairingRemoved);
    assert_eq!(canceled.effects, vec![cancel_use(1, 1)]);
    assert_eq!(canceled.canceled, Some(IntentCancelReason::PairingRemoved));
}

#[test]
fn late_authorization_for_canceled_intent_does_not_start() {
    let mut world = World::new();
    world.set(1, ended_held(1, EndReason::ServerRestart));
    world.set(2, ended_held(1, EndReason::ServerRestart));
    let mut intents = StartupIntents::new();
    world.begin(&mut intents, target(1, 1), SELECT).unwrap();
    world
        .measure(&mut intents, ticket(1), world.layout(1), grid(24, 80))
        .unwrap();
    world.activate(2);
    let left = world.revalidate(&mut intents);
    assert_eq!(left.effects, Vec::new());
    assert_eq!(left.canceled, Some(IntentCancelReason::LeftSelection));

    world.activate(1);
    let late = world
        .authorize(&mut intents, ticket(1), intent(1), world.fence(1, 1))
        .unwrap();
    assert_eq!(late.effects, vec![cancel_use(1, 1)]);
    assert_eq!(late.state, StartupState::Idle);
    assert_eq!(
        world
            .measure(&mut intents, ticket(1), world.layout(1), grid(24, 80))
            .unwrap()
            .effects,
        Vec::new()
    );

    // A superseded use's late intent is canceled; the newer use continues.
    world.begin(&mut intents, target(1, 1), SELECT).unwrap();
    world.focus(1, 2);
    let newer = world.begin(&mut intents, target(1, 2), SELECT).unwrap();
    assert_eq!(newer.canceled, Some(IntentCancelReason::Superseded));
    assert_eq!(
        newer.effects[1],
        request_measurement(&world, 3, target(1, 2), world.layout(1))
    );
    let late = world
        .authorize(&mut intents, ticket(2), intent(2), world.fence(1, 1))
        .unwrap();
    assert_eq!(late.effects, vec![cancel_use(2, 2)]);
    assert_eq!(
        late.state,
        StartupState::AwaitingAuthorization { measured: false }
    );
    assert_eq!(intents.pending().unwrap().ticket, ticket(3));

    // A ticket this state never issued is not a late authorization.
    assert_eq!(
        world.authorize(&mut intents, ticket(9), intent(9), world.fence(1, 1)),
        Err(ViewError::StaleIntent)
    );
}

#[test]
fn old_connection_epoch_session_or_context_cannot_complete_intent() {
    // An intent fenced to an older connection or context.
    for stale_context in [false, true] {
        let mut world = World::new();
        world.set(1, ended_held(1, EndReason::ServerRestart));
        let mut intents = StartupIntents::new();
        world.begin(&mut intents, target(1, 1), SELECT).unwrap();
        world
            .measure(&mut intents, ticket(1), world.layout(1), grid(24, 80))
            .unwrap();
        let current = world.fence(1, 1);
        let (connection, context, reason) = if stale_context {
            (
                current.connection(),
                ContextGeneration::new(0),
                IntentCancelReason::ContextChanged,
            )
        } else {
            (
                ConnectionBinding::new(client(A, 1), ConnectionGeneration::new(0)),
                current.context(),
                IntentCancelReason::ConnectionLost,
            )
        };
        let stale = EffectFence::new(
            current.session(),
            current.grant(),
            connection,
            context,
            None,
        )
        .unwrap();
        let reduction = world
            .authorize(&mut intents, ticket(1), intent(1), stale)
            .unwrap();
        assert_eq!(reduction.effects, vec![cancel_use(1, 1)]);
        assert_eq!(reduction.canceled, Some(reason));
    }

    // Reconnect, restart, Session replacement or a new native context after
    // authorization.
    type Event = fn(&mut World);
    let events: [(Event, IntentCancelReason); 4] = [
        (
            |world| {
                world.facts.connection = Some(ConnectionBinding::new(
                    client(A, 1),
                    ConnectionGeneration::new(2),
                ));
            },
            IntentCancelReason::ConnectionLost,
        ),
        (
            |world| {
                world.org.restart();
            },
            IntentCancelReason::EpochChanged,
        ),
        (
            |world| world.set(1, ended_held(2, EndReason::ServerRestart)),
            IntentCancelReason::SessionChanged,
        ),
        (
            |world| world.context = ContextGeneration::new(2),
            IntentCancelReason::ContextChanged,
        ),
    ];
    for (event, reason) in events {
        let mut world = World::new();
        let mut intents = StartupIntents::new();
        world.authorized_use(&mut intents);
        let layout = world.layout(1);
        event(&mut world);
        let reduction = world
            .measure(&mut intents, ticket(1), layout, grid(24, 80))
            .unwrap();
        assert_eq!(reduction.effects, vec![cancel_use(1, 1)]);
        assert_eq!(reduction.canceled, Some(reason));
        assert_eq!(reduction.state, StartupState::Idle);
    }
}

#[test]
fn running_pane_selection_does_not_request_duplicate_session() {
    let mut world = World::new();
    let mut intents = StartupIntents::new();
    for runtime in [
        running(1, Some(1), 1),
        Runtime {
            session: 2,
            phase: SessionPhase::Starting {
                generation: SessionGeneration::new(2),
            },
            control_generation: 1,
            control: held(1, 1),
        },
    ] {
        world.set(1, runtime);
        for source in [SELECT, START, RETRY] {
            let reduction = world.begin(&mut intents, target(1, 1), source).unwrap();
            assert_eq!(reduction.effects, Vec::new());
            assert_eq!(reduction.state, StartupState::Idle);
        }
    }

    // An unclaimed running Pane is compare-acquired, never started again.
    world.set(1, running(2, None, 2));
    let acquire = world.begin(&mut intents, target(1, 1), START).unwrap();
    assert_eq!(
        acquire.effects,
        vec![StartupEffect::AcquireControl {
            ticket: ticket(1),
            target: target(1, 1),
            context: world.context_of(target(1, 1)),
        }]
    );
    assert_eq!(acquire.state, StartupState::Submitted);
    let repeat = world.begin(&mut intents, target(1, 1), SELECT).unwrap();
    assert_eq!(repeat.effects, Vec::new());
    // An intent reported for an acquisition is canceled, never completed.
    let stray = world
        .authorize(&mut intents, ticket(1), intent(1), world.fence(1, 3))
        .unwrap();
    assert_eq!(stray.effects, vec![cancel_use(1, 1)]);

    world.set(1, running(2, Some(1), 3));
    assert_eq!(world.revalidate(&mut intents).state, StartupState::Idle);
    let owned = world.begin(&mut intents, target(1, 1), SELECT).unwrap();
    assert_eq!(owned.effects, Vec::new());
}

#[test]
fn offline_navigation_emits_no_acquisition_or_startup() {
    let mut world = World::new();
    world.set(1, ended_held(1, EndReason::ServerRestart));
    world.set(4, ended_held(1, EndReason::ServerRestart));
    let mut intents = StartupIntents::new();

    for facts in [offline(A), synchronizing(A)] {
        world.facts = facts;
        reconcile(&mut world.view, &world.org, &[], facts).unwrap();
        world.activate(2);
        world.focus(2, 4);
        world.activate(1);
        world.focus(1, 1);
        for source in [SELECT, OPEN, START, RETRY] {
            assert_eq!(
                world.begin(&mut intents, target(1, 1), source),
                Err(ViewError::NotReady)
            );
        }
        let reduction = world.revalidate(&mut intents);
        assert_eq!(reduction.effects, Vec::new());
        assert_eq!(reduction.state, StartupState::Idle);
    }
    assert_eq!(intents.use_for(pane(A, 1)), None);
    assert_eq!(intents.use_for(pane(A, 4)), None);
}

#[test]
fn end_all_sessions_requires_a_new_explicit_use() {
    let mut world = World::new();
    world.set(1, running(1, Some(1), 1));
    world.set(2, ended_held(1, EndReason::ServerRestart));
    world.focus(1, 2);
    let mut intents = StartupIntents::new();
    world.begin(&mut intents, target(1, 2), SELECT).unwrap();
    world
        .authorize(&mut intents, ticket(1), intent(1), world.fence(2, 1))
        .unwrap();

    // Reconciliation confirms End All Sessions.
    world.set(1, ended_held(1, EndReason::EndAllSessions));
    world.set(2, ended_held(1, EndReason::EndAllSessions));
    let ended = world.revalidate(&mut intents);
    assert_eq!(ended.effects, vec![cancel_use(1, 1)]);
    assert_eq!(ended.canceled, Some(IntentCancelReason::EndAllSessions));

    let measured = world
        .measure(&mut intents, ticket(1), world.layout(1), grid(24, 80))
        .unwrap();
    assert_eq!(measured.effects, Vec::new());
    world.focus(1, 1);
    for _ in 0..3 {
        assert_eq!(world.revalidate(&mut intents).effects, Vec::new());
    }
    assert_eq!(intents.state(), StartupState::Idle);

    // Only a new explicit use starts the ended Pane again.
    let fresh = world.begin(&mut intents, target(1, 1), START).unwrap();
    assert_eq!(
        fresh.effects,
        vec![
            StartupEffect::RequestUse {
                ticket: ticket(2),
                target: target(1, 1),
                source: START,
                kind: StartupSource::StartSession,
                context: world.context_of(target(1, 1)),
            },
            request_measurement(&world, 2, target(1, 1), world.layout(1)),
        ]
    );
}

#[test]
fn settings_closure_repaint_and_reconnect_do_not_restart_ended_pane() {
    let mut world = World::new();
    world.set(1, ended_held(1, EndReason::ServerRestart));
    let mut intents = StartupIntents::new();
    let fence = world.fence(1, 1);
    world.begin(&mut intents, target(1, 1), SELECT).unwrap();
    world
        .authorize(&mut intents, ticket(1), intent(1), fence)
        .unwrap();
    world
        .measure(&mut intents, ticket(1), world.layout(1), grid(24, 80))
        .unwrap();
    world.set(1, running(2, Some(1), 1));
    world.revalidate(&mut intents);
    world.set(1, ended_held(2, EndReason::EndAllSessions));

    // Settings closure, repaint and window focus.
    for _ in 0..3 {
        let reduction = world.revalidate(&mut intents);
        assert_eq!(reduction.effects, Vec::new());
    }
    let replay = world
        .measure(&mut intents, ticket(1), world.layout(1), grid(24, 80))
        .unwrap();
    assert_eq!(replay.effects, Vec::new());

    // Disconnect and reconnect on a new connection.
    let reconnected = ReplicaFacts {
        connection: Some(ConnectionBinding::new(
            client(A, 1),
            ConnectionGeneration::new(2),
        )),
        ..ready(A)
    };
    for facts in [offline(A), synchronizing(A), reconnected] {
        world.facts = facts;
        reconcile(&mut world.view, &world.org, &[], facts).unwrap();
        let reduction = world.revalidate(&mut intents);
        assert_eq!(reduction.effects, Vec::new());
        assert_eq!(reduction.state, StartupState::Idle);
    }
    assert_eq!(intents.use_for(pane(A, 1)), None);
}

#[test]
fn ticket_exhaustion_rejects_before_effects() {
    let mut world = World::new();
    world.set(1, ended_held(1, EndReason::ServerRestart));
    world.set(2, running(1, None, 1));
    let mut intents = StartupIntents::new();
    world.focus(1, 2);
    world.begin(&mut intents, target(1, 2), SELECT).unwrap();
    world.focus(1, 1);
    world.begin(&mut intents, target(1, 1), SELECT).unwrap();
    // Pane 2's acquisition is no longer in flight, but nothing has pruned it.
    world.set(
        2,
        Runtime {
            phase: SessionPhase::Failed {
                class: FailureClass::LaunchUnavailable,
            },
            ..ended_held(1, EndReason::ServerRestart)
        },
    );
    intents.set_issued_for_test(UseTicket::new(u64::MAX).unwrap());

    world.focus(1, 2);
    assert_eq!(
        world.begin(&mut intents, target(1, 2), RETRY),
        Err(ViewError::IdentityExhausted {
            counter: ViewCounter::UseTicket,
        })
    );
    let pending = intents.pending().unwrap();
    assert_eq!(pending.ticket, ticket(2));
    assert_eq!(pending.context.target(), target(1, 1));
    assert_eq!(
        intents.state(),
        StartupState::AwaitingAuthorization { measured: false }
    );
    assert_eq!(intents.use_for(pane(A, 2)).unwrap().ticket, ticket(1));
}

/// The presentation of Pane `pane_local` in Workspace 1 for `intents`.
fn presentation_of(world: &World, intents: &StartupIntents, pane_local: u64) -> PanePresentation {
    world.with(|facts| {
        let facts = PresentationFacts::observe(
            facts.view,
            facts.replica,
            ws(A, 1),
            pane(A, pane_local),
            intents,
            None,
        )
        .unwrap();
        PresentationState::new()
            .reduce(&facts)
            .unwrap()
            .presentation
    })
}

#[test]
fn selecting_an_unclaimed_recovery_pane_acquires_without_starting() {
    for phase in [
        SessionPhase::Failed {
            class: FailureClass::LaunchUnavailable,
        },
        SessionPhase::DirectoryUnavailable,
    ] {
        let recovery = Runtime {
            session: 1,
            phase,
            control_generation: 2,
            control: ControlRecord::Unclaimed,
        };
        for source in [SELECT, OPEN] {
            let mut world = World::new();
            world.set(1, recovery);
            let mut intents = StartupIntents::new();
            let selected = target(1, 1);
            let context = world.context_of(selected);

            // Selection and Tab opening compare-acquire only: no use request,
            // no measurement and no startup.
            let acquire = world.begin(&mut intents, selected, source).unwrap();
            assert_eq!(
                acquire.effects,
                vec![StartupEffect::AcquireControl {
                    ticket: ticket(1),
                    target: selected,
                    context,
                }]
            );
            assert_eq!(acquire.state, StartupState::Submitted);
            assert_eq!(context.control_generation(), ControlGeneration::new(2));
            let acquiring = intents.use_for(pane(A, 1)).unwrap();
            assert_eq!(acquiring.kind, UseKind::Acquisition);
            for repeat in [
                world.begin(&mut intents, selected, source).unwrap(),
                world.revalidate(&mut intents),
                world
                    .measure(&mut intents, ticket(1), world.layout(1), grid(24, 80))
                    .unwrap(),
            ] {
                assert_eq!(repeat.effects, Vec::new());
            }

            // The compare-acquire wins. The Pane stays in recovery until a
            // deliberate Retry.
            world.set(
                1,
                Runtime {
                    control_generation: 3,
                    control: held(1, 3),
                    ..recovery
                },
            );
            assert_eq!(world.revalidate(&mut intents).state, StartupState::Idle);
            let owned = world.begin(&mut intents, selected, source).unwrap();
            assert_eq!(owned.effects, Vec::new());
            assert!(matches!(
                presentation_of(&world, &intents, 1),
                PanePresentation::Failed { .. } | PanePresentation::DirectoryUnavailable
            ));
        }

        // Another Client wins the compare-acquire race: the selecting Client
        // presents Takeover and nothing it does afterwards takes over.
        let mut world = World::new();
        world.set(1, recovery);
        let mut intents = StartupIntents::new();
        world.begin(&mut intents, target(1, 1), SELECT).unwrap();
        world.set(
            1,
            Runtime {
                control_generation: 3,
                control: held(2, 3),
                ..recovery
            },
        );
        let lost = world.revalidate(&mut intents);
        assert_eq!(lost.effects, Vec::new());
        assert_eq!(lost.state, StartupState::Idle);
        assert_eq!(intents.use_for(pane(A, 1)), None);
        assert_eq!(
            presentation_of(&world, &intents, 1),
            PanePresentation::Takeover
        );
        for source in [SELECT, OPEN, RETRY, START] {
            let reduction = world.begin(&mut intents, target(1, 1), source).unwrap();
            assert_eq!(reduction.effects, Vec::new());
        }

        // A deliberate Retry while the acquisition is in flight still starts.
        let mut world = World::new();
        world.set(1, recovery);
        let mut intents = StartupIntents::new();
        world.begin(&mut intents, target(1, 1), SELECT).unwrap();
        let retry = world.begin(&mut intents, target(1, 1), RETRY).unwrap();
        assert_eq!(
            retry.effects,
            vec![
                StartupEffect::RequestUse {
                    ticket: ticket(2),
                    target: target(1, 1),
                    source: RETRY,
                    kind: StartupSource::StartSession,
                    context: world.context_of(target(1, 1)),
                },
                request_measurement(&world, 2, target(1, 1), world.layout(1)),
            ]
        );
        assert_eq!(intents.pending().unwrap().kind, UseKind::Startup);
    }
}

#[test]
fn reconnect_drops_a_submitted_use_so_start_session_requests_again() {
    let mut world = World::new();
    world.set(1, ended_held(1, EndReason::EndAllSessions));
    let mut intents = StartupIntents::new();
    let fence = world.fence(1, 1);
    world.begin(&mut intents, target(1, 1), START).unwrap();
    world
        .authorize(&mut intents, ticket(1), intent(1), fence)
        .unwrap();
    let submitted = world
        .measure(&mut intents, ticket(1), world.layout(1), grid(24, 80))
        .unwrap();
    assert_eq!(
        submitted.effects,
        vec![completion(1, 1, grid(24, 80), fence)]
    );
    assert_eq!(
        presentation_of(&world, &intents, 1),
        PanePresentation::Starting
    );

    // The connection drops before the server admits the completion, and a
    // new connection reconciles the Pane as still ended.
    world.facts = ReplicaFacts {
        connection: Some(ConnectionBinding::new(
            client(A, 1),
            ConnectionGeneration::new(2),
        )),
        ..ready(A)
    };
    assert_eq!(
        presentation_of(&world, &intents, 1),
        PanePresentation::Ended {
            reason: EndReason::EndAllSessions
        }
    );
    let again = world.begin(&mut intents, target(1, 1), START).unwrap();
    assert_eq!(
        again.effects,
        vec![
            StartupEffect::RequestUse {
                ticket: ticket(2),
                target: target(1, 1),
                source: START,
                kind: StartupSource::StartSession,
                context: world.context_of(target(1, 1)),
            },
            request_measurement(&world, 2, target(1, 1), world.layout(1)),
        ]
    );
    assert_eq!(
        presentation_of(&world, &intents, 1),
        PanePresentation::AwaitingMeasurement
    );

    // A late outcome of the old ticket cannot complete or disturb the new use.
    assert_eq!(intents.rejected(ticket(1)).canceled, None);
    let late = world
        .authorize(&mut intents, ticket(1), intent(1), fence)
        .unwrap();
    assert_eq!(late.effects, vec![cancel_use(1, 1)]);
    assert_eq!(intents.pending().unwrap().ticket, ticket(2));

    // Revalidation alone also forgets a use handed off on an older connection.
    let mut world = World::new();
    world.set(1, ended_held(1, EndReason::ServerRestart));
    let mut intents = StartupIntents::new();
    let fence = world.fence(1, 1);
    world.begin(&mut intents, target(1, 1), SELECT).unwrap();
    world
        .authorize(&mut intents, ticket(1), intent(1), fence)
        .unwrap();
    world
        .measure(&mut intents, ticket(1), world.layout(1), grid(24, 80))
        .unwrap();
    world.facts = offline(A);
    let dropped = world.revalidate(&mut intents);
    assert_eq!(dropped.effects, Vec::new());
    assert_eq!(dropped.state, StartupState::Idle);
    assert_eq!(intents.use_for(pane(A, 1)), None);
}

#[test]
fn a_later_use_of_another_pane_keeps_an_earlier_submitted_startup() {
    let mut world = World::new();
    world.set(1, ended_held(1, EndReason::EndAllSessions));
    world.set(2, running(1, None, 1));
    world.set(3, ended_held(1, EndReason::EndAllSessions));
    let mut intents = StartupIntents::new();
    let fence = world.fence(1, 1);
    world.begin(&mut intents, target(1, 1), START).unwrap();
    world
        .authorize(&mut intents, ticket(1), intent(1), fence)
        .unwrap();
    world
        .measure(&mut intents, ticket(1), world.layout(1), grid(24, 80))
        .unwrap();

    // Pane 2 is unclaimed and running: selecting it compare-acquires.
    world.focus(1, 2);
    let acquire = world.begin(&mut intents, target(1, 2), SELECT).unwrap();
    assert_eq!(
        acquire.effects,
        vec![StartupEffect::AcquireControl {
            ticket: ticket(2),
            target: target(1, 2),
            context: world.context_of(target(1, 2)),
        }]
    );
    let started = intents.use_for(pane(A, 1)).unwrap();
    assert_eq!(
        (started.ticket, started.kind, started.state),
        (ticket(1), UseKind::Startup, StartupState::Submitted)
    );
    assert_eq!(
        presentation_of(&world, &intents, 1),
        PanePresentation::Starting
    );

    // Re-selecting Pane 1 while its completion is in flight requests nothing.
    world.focus(1, 1);
    let reselected = world.begin(&mut intents, target(1, 1), SELECT).unwrap();
    assert_eq!(reselected.effects, Vec::new());

    // A later startup of Pane 3 in Tab 2 keeps both earlier hand-offs.
    world.activate(2);
    world.focus(2, 3);
    let third = world.fence(3, 1);
    world.begin(&mut intents, target(2, 3), OPEN).unwrap();
    world
        .authorize(&mut intents, ticket(3), intent(3), third)
        .unwrap();
    let completed = world
        .measure(&mut intents, ticket(3), world.layout(2), grid(30, 90))
        .unwrap();
    assert_eq!(
        completed.effects,
        vec![completion(3, 3, grid(30, 90), third)]
    );
    for (pane_local, value, kind) in [
        (1, 1, UseKind::Startup),
        (2, 2, UseKind::Acquisition),
        (3, 3, UseKind::Startup),
    ] {
        let in_flight = intents.use_for(pane(A, pane_local)).unwrap();
        assert_eq!(
            (in_flight.ticket, in_flight.kind, in_flight.state),
            (ticket(value), kind, StartupState::Submitted)
        );
    }
    let repeat = world.authorize(&mut intents, ticket(1), intent(1), fence);
    assert_eq!(repeat.unwrap().effects, Vec::new());

    // Each hand-off ends only when its own Pane changes.
    world.set(
        1,
        Runtime {
            session: 2,
            phase: SessionPhase::Starting {
                generation: SessionGeneration::new(2),
            },
            control_generation: 1,
            control: held(1, 1),
        },
    );
    assert_eq!(world.revalidate(&mut intents).effects, Vec::new());
    assert_eq!(intents.use_for(pane(A, 1)), None);
    assert!(intents.use_for(pane(A, 2)).is_some());
    assert!(intents.use_for(pane(A, 3)).is_some());
    assert_eq!(intents.state(), StartupState::Submitted);

    // The server's rejection of one ticket forgets only that hand-off.
    intents.rejected(ticket(2));
    assert_eq!(intents.use_for(pane(A, 2)), None);
    assert!(intents.use_for(pane(A, 3)).is_some());
}

/// Runs one reduction and adds its wall time to `spent`.
fn timed<R>(spent: &mut Duration, run: impl FnOnce() -> R) -> R {
    let started = Instant::now();
    let result = run();
    *spent += started.elapsed();
    result
}

/// Every `begin` and `revalidate` rechecks each in-flight use of the Server.
/// The recheck reads each Tab's view facts and arrangement once and finds each
/// Pane through the replica's index, so its cost does not multiply the delayed
/// uses by the Server's Panes. The organization holds the represented-Pane
/// bound, and the delayed Panes sort last among its runtime records. The
/// budget for the reductions is far above the indexed cost and far below that
/// product.
#[test]
fn delayed_acquisitions_at_the_pane_bound_are_rechecked_without_a_scan_per_use() {
    const TABS: u64 = 16;
    const DELAYED: u64 = 512;
    const BUDGET: Duration = Duration::from_secs(15);
    let tab_panes = MAX_TAB_PANES as u64;
    let mut org = Org::new(A);
    org.add_workspace(1, 1, 10_000)
        .chain(1, 10_000, 10_001, tab_panes - 1);
    for local in 2..=TABS {
        let root = local * 10_000;
        org.add_tab(1, local, root, None)
            .chain(local, root, root + 1, tab_panes - 1);
    }
    let first = TABS * 10_000;
    let delayed = first..first + DELAYED;
    for local in delayed.clone() {
        org.set_runtime(local, running(1, None, 1));
    }
    let mut view = ClientViewState::new();
    attach(&mut view, &org, ready(A), 1).unwrap();
    view.activate_tab(ws(A, 1), tab(A, TABS)).unwrap();
    let workspace = view.workspace(ws(A, 1)).unwrap();
    let represented: usize = workspace
        .tab_order()
        .iter()
        .map(|&tab| workspace.arrangement(tab).unwrap().pane_count())
        .sum();
    assert_eq!(represented, MAX_VIEW_PANES);

    let snapshot = org.snapshot();
    let replica = observe(&snapshot, &[], ready(A));
    let context = ContextGeneration::new(1);
    let mut intents = StartupIntents::new();
    let mut spent = Duration::ZERO;
    for (issued, local) in (1..).zip(delayed.clone()) {
        view.arrange(
            ws(A, 1),
            tab(A, TABS),
            ArrangementEdit::Focus(pane(A, local)),
        )
        .unwrap();
        let facts = UseFacts {
            view: &view,
            replica: &replica,
            context,
        };
        let reduction = timed(&mut spent, || {
            intents.begin(facts, target(TABS, local), SELECT)
        })
        .unwrap();
        assert!(matches!(
            reduction.effects.as_slice(),
            [StartupEffect::AcquireControl { ticket: acquired, target: selected, .. }]
                if *acquired == ticket(issued) && *selected == target(TABS, local)
        ));
        assert!(
            spent < BUDGET,
            "{issued} delayed acquisitions exceeded the budget"
        );
    }

    // A repaint rechecks every delayed acquisition and keeps it in flight.
    let facts = UseFacts {
        view: &view,
        replica: &replica,
        context,
    };
    let repaint = timed(&mut spent, || intents.revalidate(facts));
    assert_eq!(repaint.effects, Vec::new());
    for (issued, local) in (1..).zip(delayed.clone()) {
        let in_flight = intents.use_for(pane(A, local)).unwrap();
        assert_eq!(
            (in_flight.ticket, in_flight.kind, in_flight.state),
            (
                ticket(issued),
                UseKind::Acquisition,
                StartupState::Submitted
            )
        );
    }

    // Grants published for the first half end exactly those hand-offs.
    for local in first..first + DELAYED / 2 {
        org.set_runtime(local, running(1, Some(1), 2));
    }
    org.publish();
    let granted = org.snapshot();
    let replica = observe(&granted, &[], ready(A));
    let facts = UseFacts {
        view: &view,
        replica: &replica,
        context,
    };
    let publication = timed(&mut spent, || intents.revalidate(facts));
    assert_eq!(publication.effects, Vec::new());
    for local in delayed {
        let kept = intents.use_for(pane(A, local)).is_some();
        assert_eq!(kept, local >= first + DELAYED / 2, "Pane {local}");
    }
    assert!(spent < BUDGET, "the rechecks exceeded the budget");
}

/// A delayed acquisition ends when its Tab is removed, while one in a
/// surviving Tab stays in flight.
#[test]
fn a_delayed_acquisition_ends_when_its_tab_is_removed() {
    let mut world = World::new();
    world.set(1, running(1, None, 1));
    world.set(3, running(1, None, 1));
    let mut intents = StartupIntents::new();
    let first = world.begin(&mut intents, target(1, 1), SELECT).unwrap();
    assert_eq!(
        first.effects,
        vec![StartupEffect::AcquireControl {
            ticket: ticket(1),
            target: target(1, 1),
            context: world.context_of(target(1, 1)),
        }]
    );
    world.activate(2);
    world.focus(2, 3);
    let second = world.begin(&mut intents, target(2, 3), SELECT).unwrap();
    assert_eq!(
        second.effects,
        vec![StartupEffect::AcquireControl {
            ticket: ticket(2),
            target: target(2, 3),
            context: world.context_of(target(2, 3)),
        }]
    );

    world.activate(1);
    world.org.delete_tab(2).publish();
    reconcile(&mut world.view, &world.org, &[], world.facts).unwrap();
    assert_eq!(world.revalidate(&mut intents).effects, Vec::new());
    assert_eq!(intents.use_for(pane(A, 3)), None);
    let kept = intents.use_for(pane(A, 1)).unwrap();
    assert_eq!(
        (kept.ticket, kept.kind, kept.state),
        (ticket(1), UseKind::Acquisition, StartupState::Submitted)
    );
}

/// Client-core may lend a snapshot whose runtime records follow any order.
/// Each Pane's explicit use and presentation read that Pane's own record.
#[test]
fn runtime_records_in_any_order_describe_their_own_pane() {
    let mut world = World::new();
    world.set(1, running(1, None, 1));
    world.set(2, ended_held(3, EndReason::ServerRestart));
    world.set(3, running(2, Some(2), 4));
    let snapshot = world.org.snapshot();
    let mut runtime = snapshot.runtime().to_vec();
    runtime.reverse();
    let reordered =
        OrganizationSnapshot::new(snapshot.epoch(), snapshot.state().clone(), runtime).unwrap();
    assert_ne!(reordered.runtime(), snapshot.runtime());

    let intents = StartupIntents::new();
    let read = |snapshot: &OrganizationSnapshot| {
        let replica = observe(snapshot, &[], world.facts);
        let uses: Vec<UseContext> = [1, 2]
            .into_iter()
            .map(|local| {
                UseContext::observe(&world.view, &replica, target(1, local), world.context).unwrap()
            })
            .collect();
        let presented: Vec<PresentationFacts> = (1..=4)
            .map(|local| {
                PresentationFacts::observe(
                    &world.view,
                    &replica,
                    ws(A, 1),
                    pane(A, local),
                    &intents,
                    None,
                )
                .unwrap()
            })
            .collect();
        (uses, presented)
    };
    let (uses, presented) = read(&reordered);
    assert_eq!((uses.clone(), presented), read(&snapshot));
    assert_eq!(
        (uses[0].phase(), uses[0].control()),
        (
            SessionPhase::Running {
                generation: SessionGeneration::new(1),
            },
            ControlRecord::Unclaimed
        )
    );
    assert_eq!(
        (uses[1].session().generation, uses[1].control()),
        (SessionGeneration::new(3), held(1, 1))
    );
}
