use crate::failure::{ClientFailure, resource_limit};
use spaceterm_core::identity::ServerId;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

/// Residency follows the allocation, including cancelled work and external leases.
#[derive(Debug)]
pub struct Residency {
    server_limit: u64,
    registry_limit: u64,
    held: Mutex<Held>,
}
#[derive(Default, Debug)]
struct Held {
    total: u64,
    servers: BTreeMap<ServerId, u64>,
}
impl Residency {
    pub fn new(server_limit: u64, registry_limit: u64) -> Result<Arc<Self>, ClientFailure> {
        if server_limit == 0 || server_limit > registry_limit {
            return Err(resource_limit());
        }
        Ok(Arc::new(Self {
            server_limit,
            registry_limit,
            held: Mutex::new(Held::default()),
        }))
    }
    pub(crate) fn within(&self, server: u64, registry: u64) -> bool {
        self.server_limit <= server && self.registry_limit <= registry
    }
    pub fn held_bytes(&self) -> Result<u64, ClientFailure> {
        Ok(self.held.lock().map_err(|_| resource_limit())?.total)
    }
    pub(crate) fn reserve(
        self: &Arc<Self>,
        server: ServerId,
        bytes: u64,
    ) -> Result<Charge, ClientFailure> {
        let mut held = self.held.lock().map_err(|_| resource_limit())?;
        let total = held.total.checked_add(bytes).ok_or_else(resource_limit)?;
        let per_server = held
            .servers
            .get(&server)
            .copied()
            .unwrap_or(0)
            .checked_add(bytes)
            .ok_or_else(resource_limit)?;
        if total > self.registry_limit || per_server > self.server_limit {
            return Err(resource_limit());
        }
        held.total = total;
        held.servers.insert(server, per_server);
        Ok(Charge {
            owner: Arc::clone(self),
            server,
            bytes,
        })
    }
}
#[derive(Debug)]
pub(crate) struct Charge {
    owner: Arc<Residency>,
    server: ServerId,
    bytes: u64,
}
impl Charge {
    pub(crate) fn grow(&mut self, bytes: u64) -> Result<(), ClientFailure> {
        let mut extra = self.owner.reserve(self.server, bytes)?;
        self.bytes = self.bytes.checked_add(bytes).ok_or_else(resource_limit)?;
        extra.bytes = 0;
        Ok(())
    }
}
impl Drop for Charge {
    fn drop(&mut self) {
        if let Ok(mut held) = self.owner.held.lock() {
            held.total -= self.bytes;
            if let Some(bytes) = held.servers.get_mut(&self.server) {
                *bytes -= self.bytes;
                if *bytes == 0 {
                    held.servers.remove(&self.server);
                }
            }
        }
    }
}
