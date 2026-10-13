use crate::failure::{ClientFailure, resource_limit};
use spaceterm_core::identity::ServerId;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
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
    shared: Vec<SharedCharge>,
}
#[derive(Debug)]
struct SharedCharge {
    bytes: Weak<[u8]>,
    server: ServerId,
    charged: u64,
}
impl Held {
    fn release(&mut self, server: ServerId, bytes: u64) {
        self.total -= bytes;
        if let Some(held) = self.servers.get_mut(&server) {
            *held -= bytes;
            if *held == 0 {
                self.servers.remove(&server);
            }
        }
    }
    fn reap_shared(&mut self) {
        let mut index = 0;
        while index < self.shared.len() {
            if self.shared[index].bytes.strong_count() == 0 {
                let charge = self.shared.swap_remove(index);
                self.release(charge.server, charge.charged);
            } else {
                index += 1;
            }
        }
    }
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
        let mut held = self.held.lock().map_err(|_| resource_limit())?;
        held.reap_shared();
        Ok(held.total)
    }
    pub(crate) fn reserve(
        self: &Arc<Self>,
        server: ServerId,
        bytes: u64,
    ) -> Result<Charge, ClientFailure> {
        let mut held = self.held.lock().map_err(|_| resource_limit())?;
        held.reap_shared();
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
        if per_server != 0 {
            held.servers.insert(server, per_server);
        }
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
    /// A plain Arc<[u8]> cannot carry a Drop guard. Weak ownership keeps its
    /// accounting conservative until the next budget observation or admission.
    pub(crate) fn retain_shared(mut self, bytes: &Arc<[u8]>) -> Result<(), ClientFailure> {
        let mut held = self.owner.held.lock().map_err(|_| resource_limit())?;
        held.reap_shared();
        // Bound the accounting metadata independently of tiny graphics payloads.
        if held.shared.len() >= 8192 {
            return Err(resource_limit());
        }
        held.shared.try_reserve(1).map_err(|_| resource_limit())?;
        held.shared.push(SharedCharge {
            bytes: Arc::downgrade(bytes),
            server: self.server,
            charged: self.bytes,
        });
        self.bytes = 0;
        Ok(())
    }
    pub(crate) fn reserve_shared(&self, bytes: u64) -> Result<Charge, ClientFailure> {
        self.owner.reserve(self.server, bytes)
    }
    pub(crate) fn grow(&mut self, bytes: u64) -> Result<(), ClientFailure> {
        let mut extra = self.owner.reserve(self.server, bytes)?;
        self.bytes = self.bytes.checked_add(bytes).ok_or_else(resource_limit)?;
        extra.bytes = 0;
        Ok(())
    }
}
impl Drop for Charge {
    fn drop(&mut self) {
        if self.bytes != 0
            && let Ok(mut held) = self.owner.held.lock()
        {
            held.release(self.server, self.bytes);
        }
    }
}
