#![cfg(feature = "test-support")]
#[path = "projection/support.rs"]
mod support;
use spaceterm_client::{
    operations::{OperationPeer, OperationTracker},
    view::{launch::*, *},
};
use spaceterm_core::{
    authority::ControlGrant,
    control::ReleaseControlRequest,
    operations::{Command, OperationLookup},
    organization::ControlRecord,
};
use support::*;

fn ready<'a>(
    fixture: &Fixture,
    snapshot: &'a spaceterm_core::organization::OrganizationSnapshot,
) -> ReplicaObservation<'a> {
    let info = fixture.pane.state().borrow().connection.unwrap();
    ReplicaObservation::new(
        snapshot,
        &[],
        ReplicaFacts {
            pairing: fixture.server.binding(1).unwrap().target().pairing(),
            client: info.client,
            connection: Some(info.binding),
            readiness: ReplicaReadiness::Ready,
            status: ServerViewStatus::Connected,
        },
    )
    .unwrap()
}
async fn view(fixture: &Fixture) -> ClientViewState {
    let snapshot = fixture.server.snapshot().unwrap();
    let replica = ready(fixture, &snapshot);
    let (mut view, _) =
        ClientViewState::restore(None, &fixture.peer.journal().snapshot().unwrap()).unwrap();
    attach(
        &mut view,
        &replica,
        fixture.workspace,
        &fixture.server.binding(1).unwrap(),
        fixture.peer.journal(),
        fixture.channels.as_ref(),
    )
    .await
    .unwrap();
    view
}
#[tokio::test]
async fn view_startup_effects_preserve_exact_token_and_tagged_measurement() {
    let fixture = Fixture::new(false).await;
    let view = view(&fixture).await;
    let snapshot = fixture.server.snapshot().unwrap();
    let replica = ready(&fixture, &snapshot);
    let facts = UseFacts {
        view: &view,
        replica: &replica,
        context: fixture.pane.state().borrow().context,
    };
    let target = SelectionTarget::focused(&view, fixture.workspace).unwrap();
    let mut intents = StartupIntents::new();
    let reduction = intents
        .begin(
            facts,
            target,
            ExplicitUseSource::StartSession(SelectionSource::Command),
        )
        .unwrap();
    let binding = fixture.server.binding(1).unwrap();
    let tracker = OperationTracker::new(fixture.peer.journal().clone());
    let mut executor = StartupExecutor::new();
    let mut authorized = None;
    let mut measurement = None;
    for effect in reduction.effects {
        match executor
            .execute(
                effect,
                &fixture.pane,
                &binding,
                &tracker,
                fixture.server.as_ref(),
            )
            .await
            .unwrap()
        {
            StartupExecution::Authorized(value) => authorized = Some(value),
            StartupExecution::Measurement(value) => measurement = Some(value),
            _ => panic!("startup requested no other effect"),
        }
    }
    let authorized = authorized.unwrap();
    let request = measurement.unwrap();
    assert_eq!(authorized.ticket, request.ticket);
    let snapshot = fixture.server.snapshot().unwrap();
    let replica = ready(&fixture, &snapshot);
    let facts = UseFacts {
        view: &view,
        replica: &replica,
        context: fixture.pane.state().borrow().context,
    };
    assert!(
        intents
            .authorized(facts, authorized)
            .unwrap()
            .effects
            .is_empty()
    );
    let measured = intents
        .measured(
            facts,
            Measurement {
                ticket: request.ticket,
                context: request.context,
                layout: request.layout,
                grid: grid(),
            },
        )
        .unwrap();
    assert_eq!(measured.effects.len(), 1);
    let effect = measured.effects[0];
    let StartupExecution::Submitted(ticket) = executor
        .execute(
            effect,
            &fixture.pane,
            &binding,
            &tracker,
            fixture.server.as_ref(),
        )
        .await
        .unwrap()
    else {
        panic!("completion must be tracked")
    };
    let state = fixture.peer.journal().snapshot().unwrap();
    let operation = state.operation(ticket.id()).unwrap();
    let request = spaceterm_core::operation_ledger_contracts::CanonicalOperationRequest::decode(
        &operation.request,
        fixture.peer.journal().codec(),
    )
    .unwrap();
    assert!(
        matches!(request.command(), Command::CompleteMeasuredStartup(measurement) if measurement.intent == authorized.intent && measurement.fence == authorized.fence && measurement.grid == grid().into())
    );
    assert!(
        executor
            .execute(
                effect,
                &fixture.pane,
                &binding,
                &tracker,
                fixture.server.as_ref()
            )
            .await
            .is_err()
    );
}
#[tokio::test]
async fn view_cancel_use_cancels_original_channel_intent_without_completion() {
    let fixture = Fixture::new(false).await;
    let view = view(&fixture).await;
    let snapshot = fixture.server.snapshot().unwrap();
    let replica = ready(&fixture, &snapshot);
    let facts = UseFacts {
        view: &view,
        replica: &replica,
        context: fixture.pane.state().borrow().context,
    };
    let mut intents = StartupIntents::new();
    let effects = intents
        .begin(
            facts,
            SelectionTarget::focused(&view, fixture.workspace).unwrap(),
            ExplicitUseSource::PaneSelection(SelectionSource::Pointer),
        )
        .unwrap()
        .effects;
    let binding = fixture.server.binding(1).unwrap();
    let tracker = OperationTracker::new(fixture.peer.journal().clone());
    let mut executor = StartupExecutor::new();
    let request = effects
        .into_iter()
        .find(|effect| matches!(effect, StartupEffect::RequestUse { .. }))
        .unwrap();
    let StartupExecution::Authorized(authorization) = executor
        .execute(
            request,
            &fixture.pane,
            &binding,
            &tracker,
            fixture.server.as_ref(),
        )
        .await
        .unwrap()
    else {
        panic!("intent must be authorized")
    };
    executor
        .execute(
            StartupEffect::CancelUse {
                ticket: authorization.ticket,
                intent: authorization.intent,
            },
            &fixture.pane,
            &binding,
            &tracker,
            fixture.server.as_ref(),
        )
        .await
        .unwrap();
    assert_eq!(
        fixture.peer.cancelled_intents().unwrap()[0].id(),
        authorization.intent
    );
    assert!(
        !fixture
            .peer
            .journal()
            .snapshot()
            .unwrap()
            .operations()
            .iter()
            .any(|record| matches!(
                spaceterm_core::operation_ledger_contracts::CanonicalOperationRequest::decode(
                    &record.request,
                    fixture.peer.journal().codec()
                )
                .unwrap()
                .command(),
                Command::CompleteMeasuredStartup(_)
            ))
    );
}
#[tokio::test]
async fn view_acquire_control_uses_tracked_compare_without_startup() {
    let fixture = Fixture::new(true).await;
    let runtime = fixture.pane.state().borrow().runtime.unwrap();
    let info = fixture.pane.state().borrow().connection.unwrap();
    let ControlRecord::Held { client, generation } = runtime.control else {
        panic!("creator has control")
    };
    fixture
        .peer
        .submit(
            Command::ReleaseControl(ReleaseControlRequest {
                epoch: info.epoch,
                grant: ControlGrant::new(fixture.pane.pane(), client, generation).unwrap(),
            }),
            Some(fixture.workspace),
        )
        .await
        .unwrap();
    let view = view(&fixture).await;
    let snapshot = fixture.server.snapshot().unwrap();
    let replica = ready(&fixture, &snapshot);
    let facts = UseFacts {
        view: &view,
        replica: &replica,
        context: fixture.pane.state().borrow().context,
    };
    let mut intents = StartupIntents::new();
    let reduction = intents
        .begin(
            facts,
            SelectionTarget::focused(&view, fixture.workspace).unwrap(),
            ExplicitUseSource::PaneSelection(SelectionSource::Command),
        )
        .unwrap();
    assert_eq!(reduction.effects.len(), 1);
    assert!(matches!(
        reduction.effects[0],
        StartupEffect::AcquireControl { .. }
    ));
    let tracker = OperationTracker::new(fixture.peer.journal().clone());
    let StartupExecution::Submitted(ticket) = StartupExecutor::new()
        .execute(
            reduction.effects[0],
            &fixture.pane,
            &fixture.server.binding(1).unwrap(),
            &tracker,
            fixture.server.as_ref(),
        )
        .await
        .unwrap()
    else {
        panic!("acquisition must be tracked")
    };
    let state = fixture.peer.journal().snapshot().unwrap();
    let operation = state.operation(ticket.id()).unwrap();
    let request = spaceterm_core::operation_ledger_contracts::CanonicalOperationRequest::decode(
        &operation.request,
        fixture.peer.journal().codec(),
    )
    .unwrap();
    assert!(matches!(request.command(), Command::CompareAcquire(_)));
    assert!(matches!(
        fixture.server.lookup(ticket.binding()).await.unwrap(),
        OperationLookup::Completed { .. }
    ));
}

#[tokio::test]
async fn detached_view_acquire_callback_cannot_allocate_or_regain_control() {
    let fixture = Fixture::new(true).await;
    let runtime = fixture.pane.state().borrow().runtime.unwrap();
    let info = fixture.pane.state().borrow().connection.unwrap();
    let ControlRecord::Held { client, generation } = runtime.control else {
        panic!("creator has control")
    };
    fixture
        .peer
        .submit(
            Command::ReleaseControl(ReleaseControlRequest {
                epoch: info.epoch,
                grant: ControlGrant::new(fixture.pane.pane(), client, generation).unwrap(),
            }),
            Some(fixture.workspace),
        )
        .await
        .unwrap();
    let view = view(&fixture).await;
    let snapshot = fixture.server.snapshot().unwrap();
    let replica = ready(&fixture, &snapshot);
    let facts = UseFacts {
        view: &view,
        replica: &replica,
        context: fixture.pane.state().borrow().context,
    };
    let mut intents = StartupIntents::new();
    let reduction = intents
        .begin(
            facts,
            SelectionTarget::focused(&view, fixture.workspace).unwrap(),
            ExplicitUseSource::PaneSelection(SelectionSource::Command),
        )
        .unwrap();
    assert_eq!(reduction.effects.len(), 1);
    assert!(matches!(
        reduction.effects[0],
        StartupEffect::AcquireControl { .. }
    ));
    let tracker = OperationTracker::new(fixture.peer.journal().clone());
    let before = fixture
        .peer
        .journal()
        .snapshot()
        .unwrap()
        .operations()
        .len();
    fixture.lease.invalidate().unwrap();
    assert!(
        StartupExecutor::new()
            .execute(
                reduction.effects[0],
                &fixture.pane,
                &fixture.server.binding(1).unwrap(),
                &tracker,
                fixture.server.as_ref(),
            )
            .await
            .is_err()
    );
    assert_eq!(
        fixture
            .peer
            .journal()
            .snapshot()
            .unwrap()
            .operations()
            .len(),
        before
    );
    assert_eq!(
        fixture.server.snapshot().unwrap().runtime()[0].control,
        ControlRecord::Unclaimed
    );
}

async fn reject_retained_request_use(transition: u8) {
    use spaceterm_client::{
        limits::ClientLimits,
        pane_channel::PaneContextFacts,
        replica::{OrganizationResidency, Replica},
    };
    use spaceterm_core::organization::*;
    use std::sync::Arc;
    {
        let fixture = Fixture::new(false).await;
        let mut view = view(&fixture).await;
        let snapshot = fixture.server.snapshot().unwrap();
        let replica = ready(&fixture, &snapshot);
        let mut intents = StartupIntents::new();
        let mut effect = intents
            .begin(
                UseFacts {
                    view: &view,
                    replica: &replica,
                    context: fixture.pane.state().borrow().context,
                },
                SelectionTarget::focused(&view, fixture.workspace).unwrap(),
                ExplicitUseSource::PaneSelection(SelectionSource::Pointer),
            )
            .unwrap()
            .effects
            .into_iter()
            .find(|effect| matches!(effect, StartupEffect::RequestUse { .. }))
            .unwrap();
        let mut channel = fixture.pane.clone();
        let binding = fixture.server.binding(1).unwrap();
        if transition == 2 {
            let old_view = view.clone();
            let plan = view.prepare_detach(fixture.workspace).unwrap();
            view.apply_detach(plan).unwrap();
            let (lease, _) = attach(
                &mut view,
                &replica,
                fixture.workspace,
                &binding,
                fixture.peer.journal(),
                &fixture.channels,
            )
            .await
            .unwrap();
            channel = lease.pane(fixture.pane.pane()).unwrap();
            channel
                .update_context(PaneContextFacts {
                    focused_pane: true,
                    terminal_input_focus: true,
                    presentable: true,
                })
                .unwrap();
            // Other comparison facts can coincide; incarnation must still reject it.
            let StartupEffect::RequestUse {
                ticket,
                target,
                kind,
                source,
                ..
            } = effect
            else {
                unreachable!()
            };
            effect = StartupEffect::RequestUse {
                ticket,
                target,
                kind,
                source,
                context: UseContext::observe(
                    &old_view,
                    &replica,
                    target,
                    channel.state().borrow().context,
                )
                .unwrap(),
            };
        } else {
            let mut state = snapshot.state().clone();
            state.revision = state.revision.checked_successor().unwrap();
            let mut runtime = snapshot.runtime().to_vec();
            if transition == 0 {
                runtime[0].session_generation =
                    runtime[0].session_generation.checked_successor().unwrap();
                runtime[0].phase = SessionPhase::Ended {
                    reason: EndReason::EndAllSessions,
                };
            } else {
                runtime[0].control_generation =
                    runtime[0].control_generation.checked_successor().unwrap();
                runtime[0].control = ControlRecord::Held {
                    client: binding.target().client(),
                    generation: runtime[0].control_generation,
                };
            }
            let mut retained = state.retained.as_ref().unwrap().parts().clone();
            retained.pane_generations = runtime
                .iter()
                .map(|r| {
                    spaceterm_core::organization_retained::PaneGenerationHighWater::new(
                        r.pane,
                        r.session_generation,
                        r.control_generation,
                    )
                })
                .collect();
            state.retained = Some(
                spaceterm_core::organization_retained::OrganizationRetainedFacts::new(retained)
                    .unwrap(),
            );
            let changed = OrganizationSnapshot::new(snapshot.epoch(), state, runtime).unwrap();
            let mut publication = Replica::new(
                binding.target().server(),
                Arc::new(OrganizationResidency::new(ClientLimits::default()).unwrap()),
            );
            let token = publication.begin_resync(changed.epoch()).unwrap();
            publication.install_snapshot(token, changed).unwrap();
            let retained = publication.subscribe().borrow().retained().unwrap().clone();
            let mut info = fixture.pane.state().borrow().connection.unwrap();
            info.organization_revision = retained.cursor.revision;
            fixture.channels.publish(info, retained).unwrap();
        }
        let before = fixture
            .peer
            .journal()
            .snapshot()
            .unwrap()
            .operations()
            .len();
        let sent = fixture.server.received_operations().unwrap().len();
        assert!(
            StartupExecutor::new()
                .execute(
                    effect,
                    &channel,
                    &binding,
                    &OperationTracker::new(fixture.peer.journal().clone()),
                    fixture.server.as_ref()
                )
                .await
                .is_err(),
            "obsolete transition {transition} must reject use"
        );
        assert_eq!(
            fixture
                .peer
                .journal()
                .snapshot()
                .unwrap()
                .operations()
                .len(),
            before
        );
        assert_eq!(fixture.server.received_operations().unwrap().len(), sent);
    }
}

#[tokio::test]
async fn attachment_retry_after_channel_capacity_failure_uses_new_durable_incarnation() {
    use spaceterm_client::{
        pane_channel::PaneChannels, projection::ProjectionBudgets, view::AttachmentIncarnation,
    };
    use spaceterm_core::identity::WorkspaceId;
    let fixture = Fixture::new(false).await;
    let channels = PaneChannels::new(
        fixture.peer.clone(),
        ProjectionBudgets::supported().unwrap(),
    )
    .unwrap();
    let info = fixture.pane.state().borrow().connection.unwrap();
    channels
        .publish(info, fixture.server.replica().unwrap())
        .unwrap();
    let leases: Vec<_> = (2..=4097)
        .map(|id| {
            channels
                .attach(
                    WorkspaceId::new(info.server, id).unwrap(),
                    AttachmentIncarnation::FIRST,
                )
                .unwrap()
        })
        .collect();
    let snapshot = fixture.server.snapshot().unwrap();
    let replica = ready(&fixture, &snapshot);
    let journal = fixture.peer.journal();
    let (mut view, _) = ClientViewState::restore(None, &journal.snapshot().unwrap()).unwrap();
    let binding = fixture.server.binding(1).unwrap();
    assert!(
        attach(
            &mut view,
            &replica,
            fixture.workspace,
            &binding,
            journal,
            &channels
        )
        .await
        .is_err()
    );
    assert!(view.attachments().is_empty());
    drop(leases);
    let (lease, _) = attach(
        &mut view,
        &replica,
        fixture.workspace,
        &binding,
        journal,
        &channels,
    )
    .await
    .unwrap();
    assert_eq!(lease.incarnation().get(), 2);
    assert!(lease.pane(fixture.pane.pane()).is_ok());
}

#[tokio::test]
async fn retained_request_use_rejects_end_all_session_transition() {
    reject_retained_request_use(0).await;
}
#[tokio::test]
async fn retained_request_use_rejects_control_generation_transition() {
    reject_retained_request_use(1).await;
}
#[tokio::test]
async fn retained_request_use_rejects_reattachment_incarnation() {
    reject_retained_request_use(2).await;
}

struct AttachmentOwner(std::sync::Arc<spaceterm_client::pane_channel::PaneChannels>);
impl spaceterm_client::pending::LocalUseInvalidator for AttachmentOwner {
    fn invalidate_workspace(
        &self,
        _: &spaceterm_core::pairing::LocalPairingKey,
        _: spaceterm_core::identity::WorkspaceId,
    ) -> Result<
        Vec<spaceterm_core::authority::GrantedControl>,
        spaceterm_client::failure::ClientFailure,
    > {
        Err(spaceterm_client::failure::ClientFailure::new(
            spaceterm_client::failure::ClientFailureClass::StaleAuthority,
        ))
    }
    fn invalidate_attachment(
        &self,
        pairing: &spaceterm_core::pairing::LocalPairingKey,
        attachment: WorkspaceAttachment,
    ) -> Result<
        Vec<spaceterm_core::authority::GrantedControl>,
        spaceterm_client::failure::ClientFailure,
    > {
        self.0.invalidate_attachment(pairing, attachment)
    }
    fn invalidate_pairing(&self, _: &spaceterm_core::pairing::LocalPairingKey) {
        panic!("detach cannot unpair");
    }
    fn remove_pairing<'a>(
        &'a self,
        _: &'a spaceterm_core::pairing::LocalPairingKey,
    ) -> spaceterm_client::ports::ClientFuture<'a, ()> {
        Box::pin(async {
            panic!("detach cannot remove pairing");
        })
    }
}

#[tokio::test]
async fn canonical_attachment_owner_rejects_old_detach_after_reattachment_and_acquisition() {
    use spaceterm_client::{pane_channel::*, pending::PendingTasks};
    use std::sync::Arc;
    let fixture = Fixture::new(true).await;
    let mut view = view(&fixture).await;
    let old = view.workspace(fixture.workspace).unwrap().attachment();
    let binding = fixture.server.binding(1).unwrap();
    let tasks = PendingTasks::new(
        fixture.peer.journal().clone(),
        Arc::new(AttachmentOwner(fixture.channels.clone())),
    );
    tasks.detach_attachment(&binding, old).await.unwrap();
    for _ in 0..4 {
        if tasks
            .drain_detach(&binding, fixture.workspace, fixture.server.as_ref())
            .await
            .unwrap()
        {
            break;
        }
    }
    assert!(
        fixture
            .peer
            .journal()
            .snapshot()
            .unwrap()
            .detaches()
            .is_empty()
    );
    fixture.peer.publish().unwrap();
    let plan = view.prepare_detach(fixture.workspace).unwrap();
    view.apply_detach(plan).unwrap();
    let snapshot = fixture.server.snapshot().unwrap();
    let replica = ready(&fixture, &snapshot);
    let (lease, _) = attach(
        &mut view,
        &replica,
        fixture.workspace,
        &binding,
        fixture.peer.journal(),
        &fixture.channels,
    )
    .await
    .unwrap();
    assert!(lease.incarnation() > old.incarnation());
    let pane = lease.pane(fixture.pane.pane()).unwrap();
    pane.update_context(PaneContextFacts {
        focused_pane: true,
        terminal_input_focus: true,
        presentable: true,
    })
    .unwrap();
    pane.explicit_use(spaceterm_client::pane_channel::ExplicitUseSource::ExplicitSelection)
        .await
        .unwrap();
    let control = fixture.server.snapshot().unwrap().runtime()[0].control;
    assert!(
        matches!(control, ControlRecord::Held { client, .. } if client == binding.target().client())
    );
    let revision = fixture.peer.journal().snapshot().unwrap().revision();
    assert!(tasks.detach_attachment(&binding, old).await.is_err());
    assert!(
        fixture
            .channels
            .invalidate_attachment(&binding.target().local_pairing().unwrap(), old)
            .is_err()
    );
    assert!(lease.pane(pane.pane()).is_ok());
    assert_eq!(
        fixture.server.snapshot().unwrap().runtime()[0].control,
        control
    );
    assert_eq!(
        fixture.peer.journal().snapshot().unwrap().revision(),
        revision
    );
}
