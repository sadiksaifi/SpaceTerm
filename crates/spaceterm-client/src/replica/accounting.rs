use crate::{
    failure::{ClientFailure, resource_limit},
    limits::ClientLimits,
};
use spaceterm_core::{
    identity::ServerId,
    organization::{MAX_DIRECTORY_BYTES, MAX_NAME_BYTES, MAX_TEMPLATE_BYTES, OrganizationState},
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

/// Reservations follow the allocated model, including external Arc leases.
#[derive(Debug)]
pub struct OrganizationResidency {
    pub(super) limits: ClientLimits,
    held: Mutex<Held>,
}
#[derive(Default, Debug)]
struct Held {
    total: usize,
    servers: BTreeMap<ServerId, usize>,
}

impl OrganizationResidency {
    pub fn new(limits: ClientLimits) -> Result<Self, ClientFailure> {
        Ok(Self {
            limits: limits.validate()?,
            held: Mutex::new(Held::default()),
        })
    }
    pub(super) fn reserve(
        self: &Arc<Self>,
        server: ServerId,
        bytes: usize,
    ) -> Result<Reservation, ClientFailure> {
        let mut held = self.held.lock().map_err(|_| resource_limit())?;
        let total = held.total.checked_add(bytes).ok_or_else(resource_limit)?;
        let per_server = held
            .servers
            .get(&server)
            .copied()
            .unwrap_or(0)
            .checked_add(bytes)
            .ok_or_else(resource_limit)?;
        if total > self.limits.organization_registry_bytes
            || per_server > self.limits.organization_server_bytes
        {
            return Err(resource_limit());
        }
        held.total = total;
        held.servers.insert(server, per_server);
        Ok(Reservation {
            owner: Arc::clone(self),
            server,
            bytes,
        })
    }
}

#[derive(Debug)]
pub(super) struct Reservation {
    owner: Arc<OrganizationResidency>,
    server: ServerId,
    bytes: usize,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if let Ok(mut held) = self.owner.held.lock() {
            held.total -= self.bytes;
            if let Some(value) = held.servers.get_mut(&self.server) {
                *value -= self.bytes;
                if *value == 0 {
                    held.servers.remove(&self.server);
                }
            }
        }
    }
}

pub(super) fn record_count(state: &OrganizationState) -> Result<usize, ClientFailure> {
    [
        state.workspaces.len(),
        state.tabs.len(),
        state.panes.len(),
        state.repositories.len(),
        state.worktrees.len(),
    ]
    .into_iter()
    .try_fold(0usize, |sum, n| {
        sum.checked_add(n).ok_or_else(resource_limit)
    })
}

/// Conservatively includes backing arrays, validation indexes and opaque bounded
/// strings. The model's private string capacities cannot be inspected by a client.
/// Twice the arrays covers growth while staging and bounded validation indexes.
pub(super) fn model_bytes(state: &OrganizationState) -> Result<usize, ClientFailure> {
    let mut bytes = 1024usize
        .checked_add(MAX_TEMPLATE_BYTES)
        .ok_or_else(resource_limit)?;
    let mut charge = |count: usize, each: usize| -> Result<(), ClientFailure> {
        bytes = bytes
            .checked_add(count.checked_mul(each).ok_or_else(resource_limit)?)
            .ok_or_else(resource_limit)?;
        Ok(())
    };
    charge(
        state.workspaces.capacity(),
        512 + MAX_NAME_BYTES + MAX_DIRECTORY_BYTES,
    )?;
    charge(state.tabs.capacity(), 512)?;
    charge(state.panes.capacity(), 768 + 2 * MAX_DIRECTORY_BYTES)?;
    charge(state.repositories.capacity(), 256 + MAX_DIRECTORY_BYTES)?;
    charge(state.worktrees.capacity(), 384 + MAX_DIRECTORY_BYTES)?;
    if let Some(retained) = &state.retained {
        let parts = retained.parts();
        charge(parts.workspace_names.capacity(), 256 + 2 * MAX_NAME_BYTES)?;
        charge(parts.creation_defaults.capacity(), 128)?;
        for default in &parts.creation_defaults {
            charge(default.replay_order().len(), 96)?;
        }
        charge(parts.pane_generations.capacity(), 128)?;
        charge(parts.tab_membership_versions.capacity(), 128)?;
        charge(parts.repository_inventory_versions.capacity(), 128)?;
        charge(parts.worktree_inventory.capacity(), 128)?;
        charge(parts.remote_access.value.endpoints().len(), 128)?;
    }
    Ok(bytes)
}
