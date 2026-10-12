//! Durable operation tracking over the frozen authenticated operation port.
//! An I/O timeout is unresolved work, never a server mutation result.

use crate::{
    api::PairedServerBinding,
    failure::{ClientFailure, ClientFailureClass, exhausted, invalid_peer},
    journal::{ClientJournalChange, JournalController, JournalOperation, RetainedOperationState},
    ports::ClientFuture,
};
use spaceterm_core::{
    authority::OrganizationRevision,
    identity::{OperationStreamId, WorkspaceId},
    operation_ledger_contracts::{CanonicalOperationRequest, CompactOutcome, StreamPurpose},
    operations::{
        Command, FieldConflict, OperationBinding, OperationId, OperationLookup, OperationSequence,
        Outcome,
    },
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::sync::watch;

/// The owning connection verifies principal, reconciliation and Workspace
/// membership, and correlates every response with the original binding/digest.
/// Implementations use the existing authenticated codec and both-major converter.
/// This port has no ordinary authentication, enrollment or native management API.
pub trait OperationPeer: Send + Sync {
    fn validate_submission(
        &self,
        binding: &PairedServerBinding,
        command: &Command,
        workspace: Option<WorkspaceId>,
    ) -> Result<(), ClientFailure>;
    fn submit<'a>(&'a self, request: &'a CanonicalOperationRequest) -> ClientFuture<'a, ()>;
    fn lookup(&self, binding: OperationBinding) -> ClientFuture<'_, OperationLookup>;
    fn acknowledge(&self, stream: OperationStreamId, through: u64) -> ClientFuture<'_, ()>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OperationStatus {
    Submitted,
    AwaitingResolution,
    Completed {
        outcome: Outcome,
        publication_revision: OrganizationRevision,
    },
    Retired,
}
#[derive(Clone)]
pub struct OperationTicket {
    binding: OperationBinding,
    status: watch::Receiver<OperationStatus>,
}
impl std::fmt::Debug for OperationTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OperationTicket(..)")
    }
}
impl OperationTicket {
    pub fn id(&self) -> OperationId {
        self.binding.id
    }
    pub fn binding(&self) -> OperationBinding {
        self.binding
    }
    pub fn status(&self) -> watch::Receiver<OperationStatus> {
        self.status.clone()
    }
}

#[derive(Clone)]
pub struct OperationTracker {
    journal: Arc<JournalController>,
    tickets: Arc<Mutex<BTreeMap<OperationId, watch::Sender<OperationStatus>>>>,
}
impl OperationTracker {
    pub fn new(journal: Arc<JournalController>) -> Self {
        Self {
            journal,
            tickets: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
    pub fn journal(&self) -> &Arc<JournalController> {
        &self.journal
    }
    pub fn ticket(&self, id: OperationId) -> Result<OperationTicket, ClientFailure> {
        let mut tickets = self.tickets.lock().map_err(|_| invalid_peer())?;
        let record = self
            .journal
            .snapshot()?
            .operation(id)
            .cloned()
            .ok_or_else(invalid_peer)?;
        let status = self.status_for(&record)?;
        let sender = tickets
            .entry(id)
            .or_insert_with(|| watch::channel(status.clone()).0);
        publish_status(sender, status);
        Ok(OperationTicket {
            binding: record.binding,
            status: sender.subscribe(),
        })
    }
    pub async fn submit(
        &self,
        binding: &PairedServerBinding,
        command: Command,
        workspace: Option<WorkspaceId>,
        peer: &dyn OperationPeer,
    ) -> Result<OperationTicket, ClientFailure> {
        peer.validate_submission(binding, &command, workspace)?;
        self.prune_tickets()?;
        let record = self
            .prepare(binding.clone(), command, workspace, false, None)
            .await?;
        let ticket = self.ticket(record.binding.id)?;
        let request = CanonicalOperationRequest::decode(&record.request, self.journal.codec())
            .map_err(|_| invalid_peer())?;
        self.journal.ensure_healthy()?;
        if !self.journal.snapshot()?.ordinary_use_allowed(binding) {
            return Ok(ticket);
        }
        if peer
            .validate_submission(binding, request.command(), workspace)
            .is_err()
        {
            return Ok(ticket);
        }
        // The operation survives send failure and caller cancellation. Recovery
        // looks it up before any resend, under exactly these retained bytes.
        if peer.submit(&request).await.is_err() {
            self.publish(ticket.id(), OperationStatus::AwaitingResolution);
        } else {
            self.publish(ticket.id(), OperationStatus::Submitted);
        }
        Ok(ticket)
    }
    pub(crate) async fn prepare(
        &self,
        binding: PairedServerBinding,
        command: Command,
        workspace: Option<WorkspaceId>,
        cleanup: bool,
        detach: Option<spaceterm_core::pairing::LocalPairingKey>,
    ) -> Result<JournalOperation, ClientFailure> {
        let journal = Arc::clone(&self.journal);
        self.journal
            .update(move |state| {
                if !state.ordinary_use_allowed(&binding) {
                    return Err(ClientFailure::new(ClientFailureClass::AuthorizationDenied));
                }
                let stream = state
                    .bindings()
                    .iter()
                    .find(|b| b.binding().is_ok_and(|saved| saved == binding))
                    .ok_or_else(invalid_peer)?
                    .streams()
                    .parts()
                    .ordinary;
                let header = state.stream(stream).ok_or_else(invalid_peer)?;
                if header.retired {
                    return Err(ClientFailure::new(ClientFailureClass::StaleAuthority));
                }
                let next = header.high_water.checked_add(1).ok_or_else(exhausted)?;
                let id = OperationId::new(
                    stream,
                    OperationSequence::new(next).map_err(|_| exhausted())?,
                );
                let request = CanonicalOperationRequest::new(id, command, journal.codec())
                    .map_err(|_| invalid_peer())?;
                let record = JournalOperation {
                    binding: request.binding(),
                    request: request
                        .encode(journal.codec())
                        .map_err(|_| invalid_peer())?,
                    workspace,
                    reserves_grant: crate::journal::model::grant_producing(request.command()),
                    cleanup,
                    state: RetainedOperationState::Pending,
                };
                let mut changes = vec![ClientJournalChange::Prepare(record.clone())];
                if let Some(key) = detach {
                    changes.push(ClientJournalChange::AddDetachRelease {
                        local_pairing: key,
                        workspace: workspace.ok_or_else(invalid_peer)?,
                        operation: record.binding,
                    });
                }
                Ok((changes, record))
            })
            .await
            .map(|(_, record)| record)
    }
    /// The connection delegate calls this again at final dequeue. A queued
    /// durable request cannot survive ordinary-use removal or stream retirement.
    pub fn admit_dispatch(
        &self,
        binding: OperationBinding,
    ) -> Result<CanonicalOperationRequest, ClientFailure> {
        let state = self.journal.snapshot()?;
        let header = state.stream(binding.id.stream()).ok_or_else(invalid_peer)?;
        if header.retired {
            return Err(ClientFailure::new(ClientFailureClass::AuthorizationDenied));
        }
        let record = state.operation(binding.id).ok_or_else(invalid_peer)?;
        if record.binding != binding || !matches!(record.state, RetainedOperationState::Pending) {
            return Err(invalid_peer());
        }
        let saved = state
            .bindings()
            .iter()
            .find(|saved| saved.streams().parts().ordinary == binding.id.stream())
            .ok_or_else(invalid_peer)?
            .binding()?;
        if !state.ordinary_use_allowed(&saved) {
            return Err(ClientFailure::new(ClientFailureClass::AuthorizationDenied));
        }
        CanonicalOperationRequest::decode(&record.request, self.journal.codec())
            .map_err(|_| invalid_peer())
    }
    pub async fn resolve(
        &self,
        id: OperationId,
        lookup: OperationLookup,
        peer: &dyn OperationPeer,
    ) -> Result<(), ClientFailure> {
        let binding = self
            .journal
            .snapshot()?
            .operation(id)
            .ok_or_else(invalid_peer)?
            .binding;
        self.resolve_bound(binding, lookup, peer).await
    }
    pub async fn resolve_bound(
        &self,
        binding: OperationBinding,
        lookup: OperationLookup,
        peer: &dyn OperationPeer,
    ) -> Result<(), ClientFailure> {
        let record = self
            .journal
            .snapshot()?
            .operation(binding.id)
            .cloned()
            .ok_or_else(invalid_peer)?;
        if record.binding != binding {
            return Err(invalid_peer());
        }
        match lookup {
            OperationLookup::Completed {
                result,
                publication_revision,
            } => {
                result
                    .validate_scope(binding.id.stream())
                    .map_err(|_| invalid_peer())?;
                let outcome = CompactOutcome::new(&result, self.journal.codec())
                    .map_err(|_| invalid_peer())?;
                if let RetainedOperationState::Completed {
                    outcome: old,
                    publication_revision: revision,
                    ..
                } = &record.state
                {
                    if *old != outcome || *revision != publication_revision {
                        return Err(invalid_peer());
                    }
                } else {
                    if let Err(error) = self
                        .journal
                        .transact(vec![ClientJournalChange::Complete {
                            binding,
                            outcome,
                            publication_revision,
                        }])
                        .await
                    {
                        self.publish(binding.id, OperationStatus::AwaitingResolution);
                        return Err(error);
                    }
                }
                self.publish(
                    binding.id,
                    OperationStatus::Completed {
                        outcome: result,
                        publication_revision,
                    },
                );
                self.acknowledge(binding.id.stream(), peer).await
            }
            OperationLookup::Pending => {
                if !matches!(record.state, RetainedOperationState::Pending) {
                    return Err(invalid_peer());
                }
                self.publish(binding.id, OperationStatus::AwaitingResolution);
                Ok(())
            }
            OperationLookup::NeverAdmitted => {
                // A completed/retired identity can never revert to new admission.
                if !matches!(record.state, RetainedOperationState::Pending) {
                    return Err(invalid_peer());
                }
                // Lookup is not dispatch authority. A different pending identity
                // may have retired this entire stream while this lookup was in flight.
                let request = self.admit_dispatch(binding)?;
                self.publish(binding.id, OperationStatus::AwaitingResolution);
                peer.submit(&request).await
            }
            OperationLookup::AcknowledgedRetired => {
                if matches!(record.state, RetainedOperationState::Pending) {
                    self.journal
                        .transact(vec![ClientJournalChange::Retire(binding)])
                        .await?;
                    // A concurrent completion may have committed before Retire.
                    // Refresh from durable state instead of retiring its result.
                    self.ticket(binding.id)?;
                }
                Ok(())
            }
            OperationLookup::SequenceGap { .. } => {
                Err(ClientFailure::new(ClientFailureClass::StaleAuthority))
            }
            OperationLookup::IdentityReusedWithDifferentContents => Err(invalid_peer()),
        }
    }
    pub async fn reconcile(
        &self,
        binding: &PairedServerBinding,
        peer: &dyn OperationPeer,
    ) -> Result<(), ClientFailure> {
        let state = self.journal.snapshot()?;
        if !state.ordinary_use_allowed(binding) {
            return Err(ClientFailure::new(ClientFailureClass::AuthorizationDenied));
        }
        let stream = state
            .bindings()
            .iter()
            .find(|b| b.binding().is_ok_and(|saved| &saved == binding))
            .ok_or_else(invalid_peer)?
            .streams()
            .parts()
            .ordinary;
        let mut records: Vec<_> = state
            .operations()
            .iter()
            .filter(|r| r.binding.id.stream() == stream)
            .cloned()
            .collect();
        records.sort_by_key(|record| record.binding.id.sequence());
        for record in records {
            self.ticket(record.binding.id)?;
            if matches!(record.state, RetainedOperationState::Pending) {
                self.publish(record.binding.id, OperationStatus::AwaitingResolution);
                let lookup = peer.lookup(record.binding).await?;
                self.resolve_bound(record.binding, lookup, peer).await?;
            }
        }
        self.acknowledge(stream, peer).await
    }
    pub async fn acknowledge(
        &self,
        stream: OperationStreamId,
        peer: &dyn OperationPeer,
    ) -> Result<(), ClientFailure> {
        let state = self.journal.snapshot()?;
        let header = state.stream(stream).ok_or_else(invalid_peer)?;
        if header.purpose != StreamPurpose::Ordinary {
            return Err(ClientFailure::new(ClientFailureClass::AuthorizationDenied));
        }
        let binding = state
            .bindings()
            .iter()
            .find(|saved| saved.streams().parts().ordinary == stream)
            .ok_or_else(|| ClientFailure::new(ClientFailureClass::AuthorizationDenied))?
            .binding()?;
        if !state.ordinary_use_allowed(&binding)
            || binding.target().local_pairing()? != header.local_pairing
        {
            return Err(ClientFailure::new(ClientFailureClass::AuthorizationDenied));
        }
        // Retirement forbids execution, not settlement of previously durable
        // results. Restricted streams still use only their revocation driver.
        if header.durable_prefix > header.confirmed_prefix {
            self.journal.ensure_healthy()?;
            peer.acknowledge(stream, header.durable_prefix).await?;
        }
        Ok(())
    }
    /// Reconciles comparison-only cleanup reservations from the authenticated
    /// replica. Ended Sessions still retain held control; phase never implies a
    /// release. Older publication callbacks cannot replace newer grant facts.
    pub async fn reconcile_grants(
        &self,
        binding: &PairedServerBinding,
        replica: &crate::replica::ReplicaSnapshot,
    ) -> Result<(), ClientFailure> {
        use spaceterm_core::{authority::GrantedControl, organization::ControlRecord};
        if replica.cursor.server != binding.target().server() {
            return Err(invalid_peer());
        }
        let state = replica.organization.state();
        let mut grants = Vec::new();
        for runtime in replica.organization.runtime() {
            if let ControlRecord::Held { client, generation } = runtime.control
                && client == binding.target().client()
            {
                let grant =
                    spaceterm_core::authority::ControlGrant::new(runtime.pane, client, generation)
                        .map_err(|_| invalid_peer())?;
                let pane = state
                    .panes
                    .iter()
                    .find(|pane| pane.id == runtime.pane)
                    .ok_or_else(invalid_peer)?;
                let tab = state
                    .tabs
                    .iter()
                    .find(|tab| tab.id == pane.tab)
                    .ok_or_else(invalid_peer)?;
                grants.push((
                    tab.workspace,
                    GrantedControl::new(replica.cursor.epoch, grant).map_err(|_| invalid_peer())?,
                ));
            }
        }
        self.journal
            .transact(vec![ClientJournalChange::ReconcileGrants {
                local_pairing: binding.target().local_pairing()?,
                epoch: replica.cursor.epoch,
                revision: replica.cursor.revision,
                grants,
            }])
            .await
            .map(|_| ())
    }
    pub async fn confirm_acknowledgement(
        &self,
        stream: OperationStreamId,
        through: u64,
    ) -> Result<(), ClientFailure> {
        self.journal
            .transact(vec![ClientJournalChange::ConfirmAcknowledgement {
                stream,
                through,
            }])
            .await
            .map(|_| ())
    }
    /// Retained original command carries the rejected draft; no current version
    /// or new connection fence is substituted during recovery or UI review.
    pub fn conflict(
        &self,
        id: OperationId,
    ) -> Result<Option<(Command, FieldConflict)>, ClientFailure> {
        let state = self.journal.snapshot()?;
        let record = state.operation(id).ok_or_else(invalid_peer)?;
        if let RetainedOperationState::Completed { outcome, .. } = &record.state
            && let Outcome::Conflict(conflict) = outcome
                .outcome(self.journal.codec())
                .map_err(|_| invalid_peer())?
        {
            let request = CanonicalOperationRequest::decode(&record.request, self.journal.codec())
                .map_err(|_| invalid_peer())?;
            return Ok(Some((request.command().clone(), conflict)));
        }
        Ok(None)
    }
    fn prune_tickets(&self) -> Result<(), ClientFailure> {
        let mut tickets = self.tickets.lock().map_err(|_| invalid_peer())?;
        let state = self.journal.snapshot()?;
        tickets.retain(|id, sender| {
            if state.operation(*id).is_some() {
                return true;
            }
            // UI consumption plus confirmed server acknowledgement is permanent
            // retirement of the local result too. Old receivers retain no draft.
            publish_status(sender, OperationStatus::Retired);
            false
        });
        Ok(())
    }
    fn status_for(&self, record: &JournalOperation) -> Result<OperationStatus, ClientFailure> {
        Ok(match &record.state {
            RetainedOperationState::Pending => OperationStatus::AwaitingResolution,
            RetainedOperationState::Completed {
                outcome,
                publication_revision,
                ..
            } => OperationStatus::Completed {
                outcome: outcome
                    .outcome(self.journal.codec())
                    .map_err(|_| invalid_peer())?,
                publication_revision: *publication_revision,
            },
            RetainedOperationState::Retired => OperationStatus::Retired,
        })
    }
    fn publish(&self, id: OperationId, status: OperationStatus) {
        if let Ok(mut tickets) = self.tickets.lock() {
            let sender = tickets
                .entry(id)
                .or_insert_with(|| watch::channel(status.clone()).0);
            publish_status(sender, status);
        }
    }
}

fn publish_status(sender: &watch::Sender<OperationStatus>, status: OperationStatus) {
    sender.send_if_modified(|current| {
        // Send and lookup futures may finish after durable completion or local
        // compaction. They cannot return a terminal ticket to a pending state.
        if matches!(current, OperationStatus::Retired)
            || (matches!(current, OperationStatus::Completed { .. })
                && matches!(
                    status,
                    OperationStatus::Submitted | OperationStatus::AwaitingResolution
                ))
            || *current == status
        {
            return false;
        }
        *current = status;
        true
    });
}
