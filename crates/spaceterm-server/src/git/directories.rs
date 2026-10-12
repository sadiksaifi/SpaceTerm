//! Owning-server directory requests and read-side observation of already committed shell facts.
use super::authority::{AuthenticatedGitPrincipal, GitOrganizationPort};
use spaceterm_core::authority::SessionKey;
use spaceterm_core::identity::{ClientId, PaneId, ServerId};
use spaceterm_core::observations::{ConfirmedDirectoryObservation, RuntimeObservationSequence};
use spaceterm_core::organization::{OrganizationSnapshot, SessionPhase};
use spaceterm_core::server_directories::*;
use std::collections::BTreeMap;
use std::sync::{Arc, Condvar, Mutex};

/// Directory scope never becomes native authority or a second organization commit path.
pub struct DirectoryService {
    server: ServerId,
    provider: Arc<dyn ServerDirectoryQueries>,
    authority: Arc<dyn GitOrganizationPort>,
    state: Arc<(Mutex<DirectoryReads>, Condvar)>,
}
#[derive(Default)]
struct DirectoryReads {
    closing: bool,
    clients: BTreeMap<ClientId, usize>,
    total: usize,
}
struct ReadLease {
    state: Arc<(Mutex<DirectoryReads>, Condvar)>,
    client: ClientId,
}
impl Drop for ReadLease {
    fn drop(&mut self) {
        let mut state = self.state.0.lock().unwrap_or_else(|e| e.into_inner());
        state.total -= 1;
        if let Some(count) = state.clients.get_mut(&self.client) {
            *count -= 1;
            if *count == 0 {
                state.clients.remove(&self.client);
            }
        }
        self.state.1.notify_all();
    }
}
impl DirectoryService {
    pub fn new(
        server: ServerId,
        provider: Arc<dyn ServerDirectoryQueries>,
        authority: Arc<dyn GitOrganizationPort>,
    ) -> Self {
        Self {
            server,
            provider,
            authority,
            state: Arc::new((Mutex::new(DirectoryReads::default()), Condvar::new())),
        }
    }
    pub async fn query(
        &self,
        principal: AuthenticatedGitPrincipal,
        request: DirectoryQuery,
    ) -> Result<DirectoryQueryResult, DirectoryFailure> {
        let context = request.parts().context.parts();
        if context.server != self.server || principal.server() != self.server {
            return Err(DirectoryFailure::WrongServer);
        }
        if context.epoch != principal.epoch()
            || context.pairing != principal.pairing()
            || context.client != principal.client()
        {
            return Err(DirectoryFailure::Unauthorized);
        }
        self.authority
            .authorize(principal)
            .await
            .map_err(|_| DirectoryFailure::Unauthorized)?;
        let lease = {
            let mut state = self
                .state
                .0
                .lock()
                .map_err(|_| DirectoryFailure::Unavailable)?;
            if state.closing {
                return Err(DirectoryFailure::Unavailable);
            }
            if state.total >= 8 || state.clients.get(&principal.client()).copied().unwrap_or(0) >= 4
            {
                return Err(DirectoryFailure::Capacity);
            }
            state.total += 1;
            *state.clients.entry(principal.client()).or_default() += 1;
            ReadLease {
                state: Arc::clone(&self.state),
                client: principal.client(),
            }
        };
        let provider = Arc::clone(&self.provider);
        let result = tokio::task::spawn_blocking(move || {
            let _lease = lease;
            provider.query(&request)
        })
        .await
        .map_err(|_| DirectoryFailure::Unavailable)??;
        // Revocation during native observation cannot return directory data to the old principal.
        self.authority
            .authorize(principal)
            .await
            .map_err(|_| DirectoryFailure::Unauthorized)?;
        Ok(result)
    }
    /// Close admission and join all owned directory observations, including dropped callers.
    pub async fn shutdown(&self) -> Result<(), DirectoryFailure> {
        let state = Arc::clone(&self.state);
        tokio::task::spawn_blocking(move || {
            let mut reads = state.0.lock().map_err(|_| DirectoryFailure::Unavailable)?;
            reads.closing = true;
            while reads.total != 0 {
                reads = state
                    .1
                    .wait(reads)
                    .map_err(|_| DirectoryFailure::Unavailable)?;
            }
            Ok(())
        })
        .await
        .map_err(|_| DirectoryFailure::Unavailable)?
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfirmedDirectoryDisposition {
    Scheduled,
    Ignored,
}
/// Only committed organization facts may update Git read interests. This type has no store port.
#[derive(Default)]
pub struct CommittedDirectoryObserver {
    sequences: BTreeMap<PaneId, (SessionKey, RuntimeObservationSequence)>,
}
impl CommittedDirectoryObserver {
    pub fn accept_confirmed_directory(
        &mut self,
        organization: &OrganizationSnapshot,
        event: &ConfirmedDirectoryObservation,
    ) -> ConfirmedDirectoryDisposition {
        let session = event.session();
        if session.epoch != organization.epoch()
            || session.pane.server() != organization.state().server
        {
            return ConfirmedDirectoryDisposition::Ignored;
        }
        let Some(runtime) = organization
            .runtime()
            .iter()
            .find(|r| r.pane == session.pane)
        else {
            return ConfirmedDirectoryDisposition::Ignored;
        };
        let Some(pane) = organization
            .state()
            .panes
            .iter()
            .find(|p| p.id == session.pane)
        else {
            return ConfirmedDirectoryDisposition::Ignored;
        };
        if runtime.phase
            != (SessionPhase::Running {
                generation: session.generation,
            })
            || runtime.session_generation != session.generation
            || pane.confirmed_restoration_directory.value.as_ref() != Some(event.directory())
        {
            return ConfirmedDirectoryDisposition::Ignored;
        }
        self.sequences
            .retain(|pane, _| organization.state().panes.iter().any(|p| p.id == *pane));
        if self
            .sequences
            .get(&session.pane)
            .is_some_and(|(old_session, sequence)| {
                *old_session == session && *sequence >= event.sequence()
            })
        {
            return ConfirmedDirectoryDisposition::Ignored;
        }
        self.sequences
            .insert(session.pane, (session, event.sequence()));
        ConfirmedDirectoryDisposition::Scheduled
    }
}
