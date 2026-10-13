//! Launch hooks for the canonical journal and personal view persistence.

use super::{
    ClientViewState, ReplicaObservation, ReplicaReadiness, ViewError, ViewPersistence, ViewStore,
    ViewUpdate,
};
use crate::{
    api::PairedServerBinding,
    failure::ClientFailure,
    journal::JournalController,
    pane_channel::{PaneChannels, WorkspaceLease},
};
use spaceterm_core::identity::WorkspaceId;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum LaunchFailure {
    #[error("client journal failed: {0}")]
    Journal(ClientFailure),
    #[error("personal view failed: {0}")]
    View(ViewError),
}
impl From<ClientFailure> for LaunchFailure {
    fn from(value: ClientFailure) -> Self {
        Self::Journal(value)
    }
}
impl From<ViewError> for LaunchFailure {
    fn from(value: ViewError) -> Self {
        Self::View(value)
    }
}

/// Run on the persistence owner, off the GUI thread for native storage. Missing
/// documents still receive durable suppression and the incarnation floor.
/// Failed loads preserve the file and remain failures, never empty defaults.
/// Retained replicas seed Worktree fallback facts before any Ready observation.
pub fn restore<S: ViewStore>(
    persistence: &mut ViewPersistence<S>,
    journal: &JournalController,
    retained: &[ReplicaObservation<'_>],
) -> Result<(ClientViewState, Vec<ViewUpdate>), LaunchFailure> {
    let loaded = persistence.load()?;
    // Native loading can outlast a suppression commit or an uncertainty latch.
    let markers = journal.snapshot()?;
    let (mut view, restored) = ClientViewState::restore(loaded.as_ref(), &markers)?;
    let mut updates = vec![restored];
    for replica in retained {
        if replica.readiness() != ReplicaReadiness::Retained {
            return Err(ViewError::NotReady.into());
        }
        updates.push(view.reconcile(replica)?);
    }
    Ok((view, updates))
}

/// The view allocates the canonical incarnation. The journal retains it before
/// a new Pane lease becomes observable. A pending detach still blocks startup
/// and grant-producing operations through the operation tracker.
pub async fn attach(
    view: &mut ClientViewState,
    replica: &ReplicaObservation<'_>,
    workspace: WorkspaceId,
    binding: &PairedServerBinding,
    journal: &JournalController,
    channels: &PaneChannels,
) -> Result<(WorkspaceLease, ViewUpdate), LaunchFailure> {
    if !journal.ordinary_use_allowed(binding)?
        || binding.target().pairing() != replica.pairing()
        || binding.target().client() != replica.client()
    {
        return Err(
            ClientFailure::new(crate::failure::ClientFailureClass::AuthorizationDenied).into(),
        );
    }
    let mut candidate = view.clone();
    let mut floor = super::RestorationFilter::default();
    if let Some(incarnation) = super::AttachmentSuppression::incarnation_floor(&journal.snapshot()?)
    {
        floor.raise_incarnation_floor(incarnation);
    }
    candidate.filter_suppressed(&floor)?;
    let update = candidate.attach(workspace, replica)?;
    let attachment = candidate
        .workspace(workspace)
        .ok_or(ViewError::TargetMissing)?
        .attachment();
    if view.workspace(workspace).is_none() {
        journal
            .record_attachment(binding.target().local_pairing()?, attachment)
            .await?;
    }
    if !journal.ordinary_use_allowed(binding)? {
        return Err(
            ClientFailure::new(crate::failure::ClientFailureClass::AuthorizationDenied).into(),
        );
    }
    let lease = channels.attach(workspace, attachment.incarnation())?;
    *view = candidate;
    Ok((lease, update))
}

use super::{AuthorizedUse, LayoutRevision, SelectionTarget, StartupEffect, UseTicket};
use crate::{
    operations::{OperationPeer, OperationTicket, OperationTracker},
    pane_channel::{ExplicitUseOutcome, IntentToken, PaneChannel, PaneReadiness},
};
use spaceterm_core::authority::{ContextGeneration, GrantedControl};
use std::collections::BTreeMap;

/// A local measurement callback, with all of the view's correlation tags.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MeasurementRequest {
    pub ticket: UseTicket,
    pub target: SelectionTarget,
    pub context: ContextGeneration,
    pub layout: LayoutRevision,
}
#[derive(Debug)]
pub enum StartupExecution {
    Authorized(AuthorizedUse),
    Controlled(GrantedControl),
    Measurement(MeasurementRequest),
    Submitted(OperationTicket),
    Cancelled,
}

/// Retains the exact PaneChannel token, never a reconstructed startup identity.
/// This owner executes reductions; the view still owns local ticket choreography
/// and the operation tracker still owns durable requests and outcomes.
#[derive(Default)]
pub struct StartupExecutor {
    intents: BTreeMap<UseTicket, (PaneChannel, IntentToken)>,
}
impl StartupExecutor {
    pub fn new() -> Self {
        Self::default()
    }
    pub async fn execute(
        &mut self,
        effect: StartupEffect,
        channel: &PaneChannel,
        binding: &PairedServerBinding,
        tracker: &OperationTracker,
        peer: &dyn OperationPeer,
    ) -> Result<StartupExecution, ClientFailure> {
        if !matches!(effect, StartupEffect::CancelUse { .. })
            && !tracker.journal().ordinary_use_allowed(binding)?
        {
            return Err(ClientFailure::new(
                crate::failure::ClientFailureClass::AuthorizationDenied,
            ));
        }
        let stale = || ClientFailure::new(crate::failure::ClientFailureClass::StaleAuthority);
        match effect {
            StartupEffect::RequestUse {
                ticket,
                target,
                kind,
                context,
                ..
            } => {
                let current = *channel.state().borrow();
                if channel.pane() != target.pane
                    || context.target() != target
                    || context.attachment().pairing() != binding.target().pairing()
                    || current.context != context.context()
                    || current.connection.is_none_or(|info| {
                        info.binding != context.connection()
                            || info.client != binding.target().client()
                    })
                    || !matches!(
                        current.readiness,
                        PaneReadiness::Live | PaneReadiness::Reconciling
                    )
                    || !current.facts.focused_pane
                    || !current.facts.presentable
                    || self.intents.contains_key(&ticket)
                {
                    return Err(stale());
                }
                channel.validate_use_context(&context)?;
                match channel.explicit_use_context(kind, context).await? {
                    ExplicitUseOutcome::ControlReady(control) => {
                        Ok(StartupExecution::Controlled(control))
                    }
                    ExplicitUseOutcome::AwaitingMeasurement(token) => {
                        let authorization = AuthorizedUse {
                            ticket,
                            intent: token.intent().id(),
                            fence: token.intent().fence(),
                        };
                        self.intents.insert(ticket, (channel.clone(), *token));
                        Ok(StartupExecution::Authorized(authorization))
                    }
                }
            }
            StartupEffect::AcquireControl {
                target, context, ..
            } => {
                let current = *channel.state().borrow();
                if channel.pane() != target.pane
                    || context.target() != target
                    || context.attachment().pairing() != binding.target().pairing()
                    || current.context != context.context()
                    || current.connection.is_none_or(|info| {
                        info.binding != context.connection()
                            || info.client != binding.target().client()
                    })
                    || current.runtime.is_none_or(|runtime| {
                        runtime.session_generation != context.session().generation
                            || runtime.control_generation != context.control_generation()
                    })
                    || !matches!(
                        current.readiness,
                        PaneReadiness::Live | PaneReadiness::Reconciling
                    )
                    || !current.facts.focused_pane
                    || !current.facts.presentable
                {
                    return Err(stale());
                }
                channel.validate_use_context(&context)?;
                let command = spaceterm_core::operations::Command::CompareAcquire(
                    spaceterm_core::control::CompareAcquireRequest {
                        epoch: context.session().epoch,
                        pane: target.pane,
                        client: binding.target().client(),
                        expected_generation: context.control_generation(),
                    },
                );
                tracker
                    .submit(binding, command, Some(target.workspace), peer)
                    .await
                    .map(StartupExecution::Submitted)
            }
            StartupEffect::RequestMeasurement {
                ticket,
                target,
                context,
                layout,
            } => {
                let current = *channel.state().borrow();
                if channel.pane() != target.pane
                    || current.context != context
                    || !matches!(
                        current.readiness,
                        PaneReadiness::Live | PaneReadiness::Reconciling
                    )
                {
                    return Err(stale());
                }
                Ok(StartupExecution::Measurement(MeasurementRequest {
                    ticket,
                    target,
                    context,
                    layout,
                }))
            }
            StartupEffect::CompleteUse {
                ticket,
                intent,
                grid,
                fence,
            } => {
                let (owner, token) = self.intents.get(&ticket).ok_or_else(stale)?;
                if owner.pane() != channel.pane()
                    || token.intent().id() != intent
                    || token.intent().fence() != fence
                {
                    return Err(stale());
                }
                let (owner, token) = self.intents.remove(&ticket).ok_or_else(stale)?;
                let completed = owner.complete_intent(&token, grid).await;
                if completed.is_err() {
                    owner.cancel_intent(&token);
                }
                completed.map(StartupExecution::Submitted)
            }
            StartupEffect::CancelUse { ticket, intent } => {
                if let Some((_, token)) = self.intents.get(&ticket) {
                    if token.intent().id() != intent {
                        return Err(stale());
                    }
                    if let Some((owner, token)) = self.intents.remove(&ticket) {
                        owner.cancel_intent(&token);
                    }
                }
                Ok(StartupExecution::Cancelled)
            }
        }
    }
}
impl Drop for StartupExecutor {
    fn drop(&mut self) {
        for (channel, token) in self.intents.values() {
            channel.cancel_intent(token);
        }
    }
}
