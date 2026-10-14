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
    borrow::Cow,
    collections::BTreeMap,
    ffi::OsStr,
    sync::{Condvar, Mutex, atomic::Ordering},
};

const MAX_CACHE_BYTES: usize = 8 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 128 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document<'a> {
    version: u32,
    pin: [u8; 32],
    pairing: LocalPairingKey,
    client: spaceterm_core::identity::ClientId,
    epoch: ServerEpoch,
    state: Cow<'a, OrganizationState>,
    runtime: Cow<'a, [PaneRuntimeRecord]>,
}

pub(in crate::platform::desktop_client_startup) struct OrganizationCache {
    paths: Arc<AppPaths>,
    pending: Mutex<
        BTreeMap<
            ServerId,
            (
                PairedServerBinding,
                Arc<spaceterm_client::replica::ReplicaSnapshot>,
            ),
        >,
    >,
    finished: Condvar,
    running: AtomicBool,
}
impl OrganizationCache {
    pub(in crate::platform::desktop_client_startup) fn new(paths: Arc<AppPaths>) -> Arc<Self> {
        Arc::new(Self {
            paths,
            pending: Mutex::new(BTreeMap::new()),
            running: AtomicBool::new(false),
            finished: Condvar::new(),
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
        let recovered = journal.snapshot()?;
        // Durable suppression also resumes optional metadata cleanup after a crash
        // between the self-unpair commit and the initiating removal hook.
        for pairing in recovered.tombstones() {
            self.remove_pairing_document(pairing)?;
        }
        let Some(directory) = self
            .paths
            .open_secure_root(AppDirectoryRoot::State)
            .map_err(|_| fail(ClientFailureClass::StorageUnavailable))?
        else {
            return Ok(vec![]);
        };
        let mut snapshots = vec![];
        let mut total = 0usize;
        for saved in recovered.bindings() {
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
                OrganizationSnapshot::new(
                    document.epoch,
                    document.state.into_owned(),
                    document.runtime.into_owned(),
                )
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
            state: Cow::Borrowed(snapshot.state()),
            runtime: Cow::Borrowed(snapshot.runtime()),
        };
        // A fixed slice is a bounded standard-library writer. Oversized metadata never
        // creates an uncharged, arbitrarily large JSON copy before checking the ceiling.
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(MAX_CACHE_BYTES)
            .map_err(|_| fail(ClientFailureClass::ResourceLimit))?;
        bytes.resize(MAX_CACHE_BYTES, 0);
        let mut output = bytes.as_mut_slice();
        serde_json::to_writer(&mut output, &document).map_err(|error| {
            fail(if error.is_io() {
                ClientFailureClass::ResourceLimit
            } else {
                ClientFailureClass::InvalidStorage
            })
        })?;
        let length = MAX_CACHE_BYTES - output.len();
        bytes.truncate(length);
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
    /// Wait for the optional writer after its connection owners have stopped publishing.
    /// Failed cache writes leave the last durable metadata; they never alter the journal.
    pub(super) async fn flush(self: &Arc<Self>) -> Result<(), ClientFailure> {
        let cache = self.clone();
        tokio::task::spawn_blocking(move || {
            let mut pending = cache
                .pending
                .lock()
                .map_err(|_| fail(ClientFailureClass::StorageUnavailable))?;
            while cache.running.load(Ordering::Acquire) || !pending.is_empty() {
                pending = cache
                    .finished
                    .wait(pending)
                    .map_err(|_| fail(ClientFailureClass::StorageUnavailable))?;
            }
            Ok(())
        })
        .await
        .map_err(|_| fail(ClientFailureClass::StorageUnavailable))?
    }
    /// A delayed cleanup removes only the original pairing's identity-checked cache file.
    pub(super) async fn remove_pairing(
        self: &Arc<Self>,
        pairing: &LocalPairingKey,
    ) -> Result<(), ClientFailure> {
        self.flush().await?;
        let cache = self.clone();
        let pairing = pairing.clone();
        tokio::task::spawn_blocking(move || cache.remove_pairing_document(&pairing))
            .await
            .map_err(|_| fail(ClientFailureClass::StorageUnavailable))?
    }
    fn remove_pairing_document(&self, pairing: &LocalPairingKey) -> Result<(), ClientFailure> {
        let Some(directory) = self
            .paths
            .open_secure_root(AppDirectoryRoot::State)
            .map_err(|_| fail(ClientFailureClass::StorageUnavailable))?
        else {
            return Ok(());
        };
        let name = Self::name(pairing.parts().server);
        let Some(file) = self
            .paths
            .filesystem()
            .read_private_file(&directory, OsStr::new(&name), MAX_CACHE_BYTES)
            .map_err(|_| fail(ClientFailureClass::StorageUnavailable))?
        else {
            return Ok(());
        };
        let document: Document = serde_json::from_slice(&file.bytes)
            .map_err(|_| fail(ClientFailureClass::InvalidStorage))?;
        if document.version != 1 {
            return Err(fail(ClientFailureClass::UnknownSchema));
        }
        if &document.pairing == pairing {
            self.paths
                .filesystem()
                .remove_private_file(&directory, OsStr::new(&name), &file.identity)
                .map_err(|_| fail(ClientFailureClass::RecoveryRequired))?;
        }
        Ok(())
    }
    /// Coalesce metadata by server behind one writer. Publication never waits for native IO.
    pub(super) fn publish(
        self: &Arc<Self>,
        binding: PairedServerBinding,
        snapshot: Arc<spaceterm_client::replica::ReplicaSnapshot>,
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
                        cache.finished.notify_all();
                    }
                    next
                };
                let Some((_, (binding, snapshot))) = next else {
                    return;
                };
                // A failed optional cache write never replaces the canonical journal or identity.
                // The previous durable metadata remains the cold-start presentation source.
                let _ = cache.save(&binding, &snapshot.organization);
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

#[cfg(test)]
mod tests {
    use super::*;
    use spaceterm_client::testing::{FakePaneConnection, FakeServer};
    use spaceterm_core::pairing::{LocalPairingGeneration, LocalPairingKeyParts};

    #[tokio::test]
    async fn organization_cache_flushes_publication_and_removes_only_original_pairing() {
        let root = crate::platform::local_server_client::tests::Root::new();
        let paths = crate::platform::client_view_store::paths_for_root(&root.0);
        let server = FakeServer::new(1, vec![2, 1]).unwrap();
        let peer = FakePaneConnection::new(server.clone(), 1, 2).await.unwrap();
        let binding = server.binding(1).unwrap();
        let key = binding.target().local_pairing().unwrap();
        let cache = OrganizationCache::new(paths.clone());
        cache.publish(binding, server.replica().unwrap()).unwrap();
        cache.flush().await.unwrap();
        let reopened = OrganizationCache::new(paths);
        assert_eq!(reopened.load(peer.journal()).unwrap().len(), 1);
        let another_generation = LocalPairingKey::new(LocalPairingKeyParts {
            generation: LocalPairingGeneration::new(key.parts().generation.get() + 1).unwrap(),
            ..key.parts().clone()
        })
        .unwrap();
        reopened.remove_pairing(&another_generation).await.unwrap();
        assert_eq!(reopened.load(peer.journal()).unwrap().len(), 1);
        reopened.remove_pairing(&key).await.unwrap();
        assert!(reopened.load(peer.journal()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn recovered_self_unpair_removes_cache_without_initiating_removal_hook() {
        let root = crate::platform::local_server_client::tests::Root::new();
        let paths = crate::platform::client_view_store::paths_for_root(&root.0);
        let server = FakeServer::new(1, vec![2, 1]).unwrap();
        let peer = FakePaneConnection::new(server.clone(), 1, 2).await.unwrap();
        let binding = server.binding(1).unwrap();
        let cache = OrganizationCache::new(paths.clone());
        cache.save(&binding, &server.snapshot().unwrap()).unwrap();
        let document = paths
            .directories()
            .root(AppDirectoryRoot::State)
            .join(OrganizationCache::name(server.server()));
        assert!(document.exists());
        use spaceterm_client::journal::{
            ClientJournalChange, JournalRevocation, RevocationPhase, SavedTarget,
        };
        use spaceterm_core::{
            operations::{Command, OperationId, OperationSequence},
            pairing::{
                PairingCommand, PairingTarget, PairingTargetParts, PendingParts,
                PendingRevocationTask, PendingRevocationTaskParts, PendingSelfRevocation,
                RevocationRouteReference,
            },
        };
        let target = binding.target();
        let saved = peer.journal().snapshot().unwrap().bindings()[0].clone();
        let operation = OperationId::new(
            saved.streams().parts().self_revocation,
            OperationSequence::new(1).unwrap(),
        );
        let command = Command::Pairing(PairingCommand::SelfRevoke {
            target: PairingTarget::new(PairingTargetParts {
                server: target.server(),
                pairing: target.pairing(),
                device: target.device(),
            })
            .unwrap(),
        });
        let request = spaceterm_core::operation_ledger_contracts::CanonicalOperationRequest::new(
            operation,
            command,
            &spaceterm_core::operation_ledger::canonical::CanonicalCodec,
        )
        .unwrap();
        let task = PendingRevocationTask::new(PendingRevocationTaskParts {
            original: PendingSelfRevocation::new(PendingParts {
                original_server: target.server(),
                original_pairing: target.pairing(),
                device: target.device(),
                client: target.client(),
                operation,
                route: RevocationRouteReference::new([9; 16]),
                credential: target.credential(),
                notice_dismissed: false,
            })
            .unwrap(),
            local_pairing: target.local_pairing().unwrap(),
            pinned_server_public_key: target.pin().public_key,
            request_digest: request.binding().digest,
        })
        .unwrap();
        peer.journal()
            .transact(vec![ClientJournalChange::BeginRevocation(Box::new(
                JournalRevocation {
                    task,
                    target: SavedTarget::from_target(target),
                    phase: RevocationPhase::AwaitingConfirmation,
                    notice_dismissed: false,
                    attempts: 0,
                },
            ))])
            .await
            .unwrap();
        drop(cache);
        let reopened = OrganizationCache::new(paths);
        assert!(reopened.load(peer.journal()).unwrap().is_empty());
        assert!(!document.exists());
        // Recovered cleanup must not remove metadata from a later local incarnation.
        let replacement = PairedServerBinding::new(
            ConnectionTarget::new(
                target.pin(),
                target.device(),
                target.pairing(),
                target.client(),
                target.route().clone(),
                target.credential(),
                LocalPairingGeneration::new(target.incarnation().get() + 1).unwrap(),
            )
            .unwrap(),
        );
        reopened
            .save(&replacement, &server.snapshot().unwrap())
            .unwrap();
        let replacement_bytes = std::fs::read(&document).unwrap();
        assert!(reopened.load(peer.journal()).unwrap().is_empty());
        assert!(std::fs::read(document).unwrap() == replacement_bytes);
    }

    #[tokio::test]
    async fn oversized_organization_cache_preserves_last_bounded_document() {
        use spaceterm_core::{
            authority::FieldVersion,
            identity::RepositoryId,
            organization::{RepositoryRecord, ServerDirectory},
            organization_retained::{OrganizationRetainedFacts, RepositoryInventoryVersion},
        };
        let root = crate::platform::local_server_client::tests::Root::new();
        let paths = crate::platform::client_view_store::paths_for_root(&root.0);
        let server = FakeServer::new(1, vec![2, 1]).unwrap();
        let peer = FakePaneConnection::new(server.clone(), 1, 2).await.unwrap();
        let binding = server.binding(1).unwrap();
        let baseline = server.snapshot().unwrap();
        let cache = OrganizationCache::new(paths);
        cache.save(&binding, &baseline).unwrap();
        let mut state = baseline.state().clone();
        let mut retained = state.retained.as_ref().unwrap().parts().clone();
        for local in 1..=1024 {
            let id = RepositoryId::new(server.server(), local).unwrap();
            state.repositories.push(RepositoryRecord {
                id,
                directory: ServerDirectory::new(server.server(), vec![b'x'; 4096]).unwrap(),
            });
            retained
                .repository_inventory_versions
                .push(RepositoryInventoryVersion::new(id, FieldVersion::new(1)));
        }
        state.retained = Some(OrganizationRetainedFacts::new(retained).unwrap());
        let oversized = OrganizationSnapshot::new(baseline.epoch(), state, vec![]).unwrap();
        assert_eq!(
            cache.save(&binding, &oversized).unwrap_err().class(),
            ClientFailureClass::ResourceLimit
        );
        let retained = cache.load(peer.journal()).unwrap();
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].state(), baseline.state());
    }
}
