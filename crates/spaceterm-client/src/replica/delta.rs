use super::{PublicationStamp, Replica, ReplicaUpdate, ResyncReason, accounting::Reservation};
use crate::failure::{ClientFailure, invalid_peer, resource_limit};
use spaceterm_core::{
    organization::{OrganizationDelta, OrganizationWrite},
    organization_retained::RetainedOrganizationWrite,
};

pub(super) struct DeltaStaging {
    stamp: PublicationStamp,
    delta: OrganizationDelta,
    reservations: Vec<Reservation>,
}

pub(super) fn write_bytes(delta: &OrganizationDelta) -> Result<usize, ClientFailure> {
    let mut bytes = 1024usize;
    for write in delta.writes() {
        let replay = match write {
            OrganizationWrite::Retained(RetainedOrganizationWrite::PutCreationDefault(v)) => {
                v.replay_order().len()
            }
            _ => 0,
        };
        bytes = bytes
            .checked_add(
                32_768usize
                    .checked_add(replay.checked_mul(96).ok_or_else(resource_limit)?)
                    .ok_or_else(resource_limit)?,
            )
            .ok_or_else(resource_limit)?;
    }
    bytes = bytes
        .checked_add(
            delta
                .runtime()
                .len()
                .checked_mul(128)
                .ok_or_else(resource_limit)?,
        )
        .ok_or_else(resource_limit)?;
    bytes = bytes
        .checked_add(
            delta
                .accepted_creations()
                .len()
                .checked_mul(256)
                .ok_or_else(resource_limit)?,
        )
        .ok_or_else(resource_limit)?;
    Ok(bytes)
}

impl Replica {
    /// Multipart deltas keep the installed model unchanged until every ordered
    /// part has arrived. There is no queue of deltas behind a missing base.
    pub fn stage_delta(
        &mut self,
        stamp: PublicationStamp,
        delta: OrganizationDelta,
    ) -> Result<ReplicaUpdate, ClientFailure> {
        let result = self.stage_delta_current(stamp, delta);
        if result.is_err() {
            self.invalidate(ResyncReason::InvalidPublication);
        }
        result
    }
    fn stage_delta_current(
        &mut self,
        stamp: PublicationStamp,
        delta: OrganizationDelta,
    ) -> Result<ReplicaUpdate, ClientFailure> {
        if !self.state.borrow().is_ready() {
            return Ok(ReplicaUpdate::Ignored);
        }
        let base = self
            .state
            .borrow()
            .retained()
            .cloned()
            .ok_or_else(invalid_peer)?;
        if delta.server() != self.server
            || stamp.cursor.server != self.server
            || delta.epoch() != stamp.cursor.epoch
            || delta.revision() != stamp.cursor.revision
            || stamp.transfer == 0
            || stamp.total == 0
            || stamp.total as usize > self.budget.limits.organization_records
            || stamp.index >= stamp.total
        {
            return Err(invalid_peer());
        }
        if base.cursor.epoch != delta.epoch() || base.cursor.revision != delta.base() {
            self.invalidate(ResyncReason::RevisionGap);
            return Ok(ReplicaUpdate::ResyncRequired);
        }
        if delta.writes().len() > self.budget.limits.organization_records
            || delta.runtime().len() > self.budget.limits.organization_records
        {
            return Err(resource_limit());
        }
        let reservation = self.budget.reserve(self.server, write_bytes(&delta)?)?;
        if let Some(staging) = self.delta_staging.take() {
            let previous = staging.stamp;
            if previous.request != stamp.request
                || previous.request_generation != stamp.request_generation
                || previous.transfer != stamp.transfer
                || previous.total != stamp.total
                || previous.cursor != stamp.cursor
                || previous.index.checked_add(1) != Some(stamp.index)
                || staging.delta.base() != delta.base()
            {
                return Err(invalid_peer());
            }
            let count = staging
                .delta
                .writes()
                .len()
                .checked_add(delta.writes().len())
                .ok_or_else(resource_limit)?;
            let runtime_count = staging
                .delta
                .runtime()
                .len()
                .checked_add(delta.runtime().len())
                .ok_or_else(resource_limit)?;
            if count > self.budget.limits.organization_records
                || runtime_count > self.budget.limits.organization_records
            {
                return Err(resource_limit());
            }
            let temporary = self
                .budget
                .reserve(self.server, write_bytes(&staging.delta)?)?;
            let mut writes = staging.delta.writes().to_vec();
            writes.extend_from_slice(delta.writes());
            let mut runtime = staging.delta.runtime().to_vec();
            runtime.extend_from_slice(delta.runtime());
            let mut creations = staging.delta.accepted_creations().to_vec();
            creations.extend_from_slice(delta.accepted_creations());
            let combined = OrganizationDelta::new(
                self.server,
                delta.epoch(),
                delta.base(),
                delta.revision(),
                writes,
                runtime,
            )
            .and_then(|d| d.with_accepted_creations(creations))
            .map_err(|_| invalid_peer())?;
            drop(temporary);
            let mut reservations = staging.reservations;
            reservations.push(reservation);
            self.delta_staging = Some(DeltaStaging {
                stamp,
                delta: combined,
                reservations,
            });
        } else {
            if stamp.index != 0 {
                return Err(invalid_peer());
            }
            self.delta_staging = Some(DeltaStaging {
                stamp,
                delta,
                reservations: vec![reservation],
            });
        }
        if stamp.index + 1 != stamp.total {
            return Ok(ReplicaUpdate::Staged);
        }
        let staging = self.delta_staging.take().ok_or_else(invalid_peer)?;
        self.apply_delta(&staging.delta)
    }
}
