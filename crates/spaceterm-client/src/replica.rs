//! Atomic, bounded authoritative organization replicas.

mod accounting;
mod apply;
mod delta;
pub use accounting::OrganizationResidency;

use crate::failure::{ClientFailure, exhausted, invalid_peer, resource_limit};
use accounting::{Reservation, model_bytes, record_count};
use spaceterm_core::{
    authority::{OrganizationRevision, ServerEpoch},
    identity::{ServerId, TabId},
    organization::{
        AcceptedPaneCreation, CreationTopologyReadModel, OrganizationDelta, OrganizationSnapshot,
        OrganizationState, OrganizationWrite, PaneRuntimeRecord,
    },
    organization_retained::{RetainedOrganizationWrite, TabCreationDefault},
};
use std::sync::Arc;
use tokio::sync::watch;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrganizationCursor {
    pub server: ServerId,
    pub epoch: ServerEpoch,
    pub revision: OrganizationRevision,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResyncReason {
    ConnectionLost,
    RevisionGap,
    EpochChanged,
    InvalidPublication,
}

#[derive(Debug)]
pub struct ReplicaSnapshot {
    pub cursor: OrganizationCursor,
    pub organization: OrganizationSnapshot,
    accepted_creations: Vec<AcceptedPaneCreation>,
    _reservations: Vec<Reservation>,
}

impl CreationTopologyReadModel for ReplicaSnapshot {
    fn server(&self) -> ServerId {
        self.cursor.server
    }
    fn epoch(&self) -> ServerEpoch {
        self.cursor.epoch
    }
    fn revision(&self) -> OrganizationRevision {
        self.cursor.revision
    }
    fn creation_default(&self, tab: TabId) -> Option<&TabCreationDefault> {
        self.organization
            .state()
            .retained
            .as_ref()?
            .creation_defaults()
            .iter()
            .find(|value| value.tab() == tab)
    }
    fn accepted_creations(&self) -> &[AcceptedPaneCreation] {
        &self.accepted_creations
    }
}

#[derive(Clone, Debug)]
pub enum ReplicaState {
    Empty,
    Ready {
        snapshot: Arc<ReplicaSnapshot>,
    },
    Stale {
        retained: Arc<ReplicaSnapshot>,
        reason: ResyncReason,
    },
    Resynchronizing {
        retained: Option<Arc<ReplicaSnapshot>>,
    },
}
impl ReplicaState {
    pub fn retained(&self) -> Option<&Arc<ReplicaSnapshot>> {
        match self {
            Self::Ready { snapshot } => Some(snapshot),
            Self::Stale { retained, .. } => Some(retained),
            Self::Resynchronizing { retained } => retained.as_ref(),
            Self::Empty => None,
        }
    }
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResyncToken(u64);
impl ResyncToken {
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Comparison fields from the protocol's PublicationPart. Parts are ordered.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublicationStamp {
    pub request: u64,
    pub request_generation: u64,
    pub transfer: u64,
    pub index: u32,
    pub total: u32,
    pub cursor: OrganizationCursor,
}

/// A bounded decoded part, not a valid or installed organization snapshot.
pub struct SnapshotPart {
    pub stamp: PublicationStamp,
    pub state: OrganizationState,
    pub runtime: Vec<PaneRuntimeRecord>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplicaUpdate {
    Installed,
    Staged,
    Ignored,
    ResyncRequired,
}

struct Staging {
    stamp: PublicationStamp,
    state: OrganizationState,
    runtime: Vec<PaneRuntimeRecord>,
    reservations: Vec<Reservation>,
}

pub struct Replica {
    server: ServerId,
    budget: Arc<OrganizationResidency>,
    state: watch::Sender<ReplicaState>,
    generation: u64,
    expected_epoch: Option<ServerEpoch>,
    staging: Option<Staging>,
    delta_staging: Option<delta::DeltaStaging>,
}
impl Replica {
    pub fn new(server: ServerId, budget: Arc<OrganizationResidency>) -> Self {
        Self {
            server,
            budget,
            state: watch::channel(ReplicaState::Empty).0,
            generation: 0,
            expected_epoch: None,
            staging: None,
            delta_staging: None,
        }
    }
    pub fn subscribe(&self) -> watch::Receiver<ReplicaState> {
        self.state.subscribe()
    }

    pub fn begin_resync(&mut self, epoch: ServerEpoch) -> Result<ResyncToken, ClientFailure> {
        if epoch.get() == 0 {
            return Err(invalid_peer());
        }
        self.generation = self.generation.checked_add(1).ok_or_else(exhausted)?;
        self.expected_epoch = Some(epoch);
        self.staging = None;
        self.delta_staging = None;
        self.delta_staging = None;
        let retained = self.state.borrow().retained().cloned();
        self.state
            .send_replace(ReplicaState::Resynchronizing { retained });
        Ok(ResyncToken(self.generation))
    }

    fn current(&self, token: ResyncToken) -> bool {
        self.expected_epoch.is_some() && token.0 == self.generation
    }

    fn validate_scope(&self, snapshot: &OrganizationSnapshot) -> Result<(), ClientFailure> {
        if snapshot.state().server != self.server || Some(snapshot.epoch()) != self.expected_epoch {
            return Err(invalid_peer());
        }
        if record_count(snapshot.state())? > self.budget.limits.organization_records {
            return Err(resource_limit());
        }
        if let Some(previous) = self.state.borrow().retained() {
            if previous.cursor.epoch == snapshot.epoch()
                && previous.cursor.revision > snapshot.state().revision
            {
                return Err(invalid_peer());
            }
            apply::validate_transition(&previous.organization, snapshot)?;
        }
        apply::validate_fields(snapshot)
    }

    fn publish(
        &mut self,
        organization: OrganizationSnapshot,
        reservations: Vec<Reservation>,
        accepted_creations: Vec<AcceptedPaneCreation>,
    ) -> Result<ReplicaUpdate, ClientFailure> {
        self.validate_scope(&organization)?;
        let cursor = OrganizationCursor {
            server: self.server,
            epoch: organization.epoch(),
            revision: organization.state().revision,
        };
        self.state.send_replace(ReplicaState::Ready {
            snapshot: Arc::new(ReplicaSnapshot {
                cursor,
                organization,
                accepted_creations,
                _reservations: reservations,
            }),
        });
        self.staging = None;
        Ok(ReplicaUpdate::Installed)
    }

    pub fn install_snapshot(
        &mut self,
        token: ResyncToken,
        snapshot: OrganizationSnapshot,
    ) -> Result<ReplicaUpdate, ClientFailure> {
        if !self.current(token) {
            return Ok(ReplicaUpdate::Ignored);
        }
        self.validate_scope(&snapshot)?;
        let reservation = self
            .budget
            .reserve(self.server, model_bytes(snapshot.state())?)?;
        self.publish(snapshot, vec![reservation], vec![])
    }

    pub fn stage_snapshot(
        &mut self,
        token: ResyncToken,
        part: SnapshotPart,
    ) -> Result<ReplicaUpdate, ClientFailure> {
        if !self.current(token) || part.stamp.request_generation != token.get() {
            return Ok(ReplicaUpdate::Ignored);
        }
        let result = self.stage_current(part);
        if result.is_err() {
            self.staging = None;
            self.invalidate(ResyncReason::InvalidPublication);
        }
        result
    }

    fn stage_current(&mut self, part: SnapshotPart) -> Result<ReplicaUpdate, ClientFailure> {
        let stamp = part.stamp;
        if stamp.request == 0
            || stamp.transfer == 0
            || stamp.total == 0
            || stamp.total as usize > self.budget.limits.organization_records
            || stamp.index >= stamp.total
            || stamp.cursor.server != self.server
            || Some(stamp.cursor.epoch) != self.expected_epoch
            || part.state.server != self.server
            || part.state.revision != stamp.cursor.revision
            || record_count(&part.state)? > self.budget.limits.organization_records
            || part.runtime.len() > self.budget.limits.organization_records
        {
            return Err(invalid_peer());
        }
        let bytes = model_bytes(&part.state)?
            .checked_add(
                part.runtime
                    .capacity()
                    .checked_mul(128)
                    .ok_or_else(resource_limit)?,
            )
            .ok_or_else(resource_limit)?;
        let reservation = self.budget.reserve(self.server, bytes)?;
        if let Some(staging) = &mut self.staging {
            let previous = staging.stamp;
            if previous.request != stamp.request
                || previous.request_generation != stamp.request_generation
                || previous.transfer != stamp.transfer
                || previous.total != stamp.total
                || previous.cursor != stamp.cursor
                || previous.index.checked_add(1) != Some(stamp.index)
            {
                return Err(invalid_peer());
            }
            let count = record_count(&staging.state)?
                .checked_add(record_count(&part.state)?)
                .ok_or_else(resource_limit)?;
            if count > self.budget.limits.organization_records {
                return Err(resource_limit());
            }
            // Charge a second model before append reallocations or retained-fact cloning.
            let temporary = self
                .budget
                .reserve(self.server, model_bytes(&staging.state)?)?;
            apply::merge(
                &mut staging.state,
                &mut staging.runtime,
                part.state,
                part.runtime,
            )?;
            drop(temporary);
            staging.reservations.push(reservation);
            staging.stamp = stamp;
        } else {
            if stamp.index != 0 {
                return Err(invalid_peer());
            }
            self.staging = Some(Staging {
                stamp,
                state: part.state,
                runtime: part.runtime,
                reservations: vec![reservation],
            });
        }
        if stamp.index + 1 != stamp.total {
            return Ok(ReplicaUpdate::Staged);
        }
        let staging = self.staging.take().ok_or_else(invalid_peer)?;
        let snapshot =
            OrganizationSnapshot::new(stamp.cursor.epoch, staging.state, staging.runtime)
                .map_err(|_| invalid_peer())?;
        self.publish(snapshot, staging.reservations, vec![])
    }

    pub fn apply_delta(
        &mut self,
        delta: &OrganizationDelta,
    ) -> Result<ReplicaUpdate, ClientFailure> {
        if self.delta_staging.is_some() {
            self.invalidate(ResyncReason::InvalidPublication);
            return Err(invalid_peer());
        }
        let Some(base) = self.state.borrow().retained().cloned() else {
            return Ok(ReplicaUpdate::ResyncRequired);
        };
        if !self.state.borrow().is_ready() {
            return Ok(ReplicaUpdate::Ignored);
        }
        if delta.server() != self.server {
            self.invalidate(ResyncReason::InvalidPublication);
            return Err(invalid_peer());
        }
        if delta.epoch() != base.cursor.epoch {
            self.invalidate(ResyncReason::EpochChanged);
            return Ok(ReplicaUpdate::ResyncRequired);
        }
        if delta.base() != base.cursor.revision || delta.revision() <= base.cursor.revision {
            self.invalidate(ResyncReason::RevisionGap);
            return Ok(ReplicaUpdate::ResyncRequired);
        }
        let result = (|| {
            if delta.writes().len() > self.budget.limits.organization_records
                || delta.runtime().len() > self.budget.limits.organization_records
            {
                return Err(resource_limit());
            }
            // Covers the candidate and its validation indexes before cloning the base.
            let mut bytes = model_bytes(base.organization.state())?;
            for write in delta.writes() {
                let replay = match write {
                    OrganizationWrite::Retained(RetainedOrganizationWrite::PutCreationDefault(
                        v,
                    )) => v.replay_order().len(),
                    _ => 0,
                };
                let added = 32_768usize
                    .checked_add(replay.checked_mul(96).ok_or_else(resource_limit)?)
                    .ok_or_else(resource_limit)?;
                bytes = bytes.checked_add(added).ok_or_else(resource_limit)?;
            }
            let reservation = self.budget.reserve(self.server, bytes)?;
            let candidate = apply::apply(&base.organization, delta)?;
            self.publish(
                candidate,
                vec![reservation],
                delta.accepted_creations().to_vec(),
            )
        })();
        if result.is_err() {
            self.invalidate(ResyncReason::InvalidPublication);
        }
        result
    }

    /// Revokes readiness without throwing away the last valid navigation model.
    pub fn invalidate(&mut self, reason: ResyncReason) {
        self.expected_epoch = None;
        self.staging = None;
        self.delta_staging = None;
        let retained = self.state.borrow().retained().cloned();
        self.state.send_replace(match retained {
            Some(retained) => ReplicaState::Stale { retained, reason },
            None => ReplicaState::Empty,
        });
    }
}
