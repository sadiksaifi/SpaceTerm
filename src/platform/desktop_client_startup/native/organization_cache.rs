//! Bounded organization metadata for cold offline navigation. No terminal content is retained.

use super::*;
use crate::platform::{app_directories::AppDirectoryRoot, secure_filesystem::SecureCommitOutcome};
use serde::{Deserialize, Serialize};
use spaceterm_client::api::PairedServerBinding;
use spaceterm_core::{
    authority::ServerEpoch,
    organization::{OrganizationSnapshot, OrganizationState, PaneRuntimeRecord},
    pairing::LocalPairingKey,
};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    sync::{Mutex, atomic::Ordering},
};

const MAX_CACHE_BYTES: usize = 8 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 128 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    pin: [u8; 32],
    pairing: LocalPairingKey,
    client: spaceterm_core::identity::ClientId,
    epoch: ServerEpoch,
    state: OrganizationState,
    runtime: Vec<PaneRuntimeRecord>,
}

pub(in crate::platform::desktop_client_startup) struct OrganizationCache {
    paths: Arc<AppPaths>,
    pending: Mutex<BTreeMap<ServerId, (PairedServerBinding, OrganizationSnapshot)>>,
    running: AtomicBool,
}
impl OrganizationCache {
    pub(in crate::platform::desktop_client_startup) fn new(paths: Arc<AppPaths>) -> Arc<Self> {
        Arc::new(Self {
            paths,
            pending: Mutex::new(BTreeMap::new()),
            running: AtomicBool::new(false),
        })
    }
    fn name(server: ServerId) -> String {
        let mut name = String::from("organization-");
        for byte in server.bytes() {
            name.push_str(&format!("{byte:02x}"));
        }
        name.push_str(".json");
        name
    }
    pub(in crate::platform::desktop_client_startup) fn load(
        &self,
        journal: &JournalController,
    ) -> Result<Vec<OrganizationSnapshot>, ClientFailure> {
        let Some(directory) = self
            .paths
            .open_secure_root(AppDirectoryRoot::State)
            .map_err(|_| fail(ClientFailureClass::StorageUnavailable))?
        else {
            return Ok(vec![]);
        };
        let mut snapshots = vec![];
        let mut total = 0usize;
        for saved in journal.snapshot()?.bindings() {
            let binding = saved.binding()?;
            if !journal.ordinary_use_allowed(&binding)? {
                continue;
            }
            let file = self
                .paths
                .filesystem()
                .read_private_file(
                    &directory,
                    OsStr::new(&Self::name(binding.target().server())),
                    MAX_CACHE_BYTES,
                )
                .map_err(|_| fail(ClientFailureClass::StorageUnavailable))?;
            let Some(file) = file else {
                continue;
            };
            total = total
                .checked_add(file.bytes.len())
                .ok_or_else(|| fail(ClientFailureClass::ResourceLimit))?;
            if total > MAX_TOTAL_BYTES {
                return Err(fail(ClientFailureClass::ResourceLimit));
            }
            let document: Document = serde_json::from_slice(&file.bytes)
                .map_err(|_| fail(ClientFailureClass::InvalidStorage))?;
            if document.version != 1 {
                return Err(fail(ClientFailureClass::UnknownSchema));
            }
            // A previous pairing's cache never grants presentation to its replacement.
            if document.pin != binding.target().pin().public_key
                || document.pairing != binding.target().local_pairing()?
                || document.client != binding.target().client()
                || document.state.server != binding.target().server()
            {
                continue;
            }
            if record_count(&document.state)
                > spaceterm_client::limits::ClientLimits::default().organization_records
            {
                return Err(fail(ClientFailureClass::ResourceLimit));
            }
            snapshots.push(
                OrganizationSnapshot::new(document.epoch, document.state, document.runtime)
                    .map_err(|_| fail(ClientFailureClass::InvalidStorage))?,
            );
        }
        Ok(snapshots)
    }
    pub(in crate::platform::desktop_client_startup) fn save(
        &self,
        binding: &PairedServerBinding,
        snapshot: &OrganizationSnapshot,
    ) -> Result<(), ClientFailure> {
        if binding.target().server() != snapshot.state().server {
            return Err(fail(ClientFailureClass::StaleAuthority));
        }
        let document = Document {
            version: 1,
            pin: binding.target().pin().public_key,
            pairing: binding.target().local_pairing()?,
            client: binding.target().client(),
            epoch: snapshot.epoch(),
            state: snapshot.state().clone(),
            runtime: snapshot.runtime().to_vec(),
        };
        let bytes =
            serde_json::to_vec(&document).map_err(|_| fail(ClientFailureClass::InvalidStorage))?;
        if bytes.len() > MAX_CACHE_BYTES {
            return Err(fail(ClientFailureClass::ResourceLimit));
        }
        let directory = self
            .paths
            .filesystem()
            .ensure_durable_private_directory(
                self.paths.directories().root(AppDirectoryRoot::State),
            )
            .map_err(|_| fail(ClientFailureClass::StorageUnavailable))?;
        let name = Self::name(snapshot.state().server);
        let previous = self
            .paths
            .filesystem()
            .read_private_file(&directory, OsStr::new(&name), MAX_CACHE_BYTES)
            .map_err(|_| fail(ClientFailureClass::StorageUnavailable))?;
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|_| fail(ClientFailureClass::Unavailable))?;
        let prepared = self
            .paths
            .filesystem()
            .prepare_private_file(&directory, OsStr::new(&name), &bytes, nonce)
            .map_err(|_| fail(ClientFailureClass::StorageUnavailable))?;
        let committed = self
            .paths
            .filesystem()
            .commit_private_file(prepared, previous.as_ref().map(|file| &file.identity))
            .map_err(|_| fail(ClientFailureClass::RecoveryRequired))?;
        match committed.outcome {
            SecureCommitOutcome::Committed => Ok(()),
            SecureCommitOutcome::Conflict => Err(fail(ClientFailureClass::StaleAuthority)),
            SecureCommitOutcome::CommittedButUnsynced => {
                Err(fail(ClientFailureClass::RecoveryRequired))
            }
        }
    }
    /// Coalesce metadata by server behind one writer. Publication never waits for native IO.
    pub(super) fn publish(
        self: &Arc<Self>,
        binding: PairedServerBinding,
        snapshot: OrganizationSnapshot,
    ) -> Result<(), ClientFailure> {
        let mut pending = self
            .pending
            .lock()
            .map_err(|_| fail(ClientFailureClass::StorageUnavailable))?;
        if pending.len() >= spaceterm_client::limits::ClientLimits::default().saved_servers
            && !pending.contains_key(&binding.target().server())
        {
            return Err(fail(ClientFailureClass::ResourceLimit));
        }
        pending.insert(binding.target().server(), (binding, snapshot));
        drop(pending);
        if self.running.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let cache = self.clone();
        tokio::task::spawn_blocking(move || {
            loop {
                let next = {
                    let Ok(mut pending) = cache.pending.lock() else {
                        return;
                    };
                    let next = pending.pop_first();
                    if next.is_none() {
                        cache.running.store(false, Ordering::Release);
                    }
                    next
                };
                let Some((_, (binding, snapshot))) = next else {
                    return;
                };
                // A failed optional cache write never replaces the canonical journal or identity.
                // The previous durable metadata remains the cold-start presentation source.
                let _ = cache.save(&binding, &snapshot);
            }
        });
        Ok(())
    }
}
fn record_count(state: &OrganizationState) -> usize {
    state.workspaces.len()
        + state.tabs.len()
        + state.panes.len()
        + state.repositories.len()
        + state.worktrees.len()
}
