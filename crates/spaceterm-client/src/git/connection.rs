//! Git composes connection-owned reads with the canonical operation tracker.
//! The operation peer is the worker's codec port, not a second ledger or route.

use super::rpc::*;
use crate::{
    api::ServerQueries,
    failure::{ClientFailureClass, invalid_peer},
    operations::{OperationPeer, OperationStatus, OperationTicket, OperationTracker},
};
use spaceterm_core::{
    operation_ledger_contracts::CanonicalOperationRequest,
    operations::{Command, OperationId},
    worktrees::{GitCommand, GitStatusTarget},
};
use std::sync::Arc;

struct Connection {
    reads: ServerQueries,
    tracker: OperationTracker,
    peer: Arc<dyn OperationPeer>,
}
impl ServerQueries {
    /// Use the same journal as this server's query admission. The caller injects
    /// the existing worker's operation peer; this facade owns no wire codec.
    pub fn git(
        &self,
        tracker: OperationTracker,
        peer: Arc<dyn OperationPeer>,
    ) -> Result<GitClient, ClientGitFailure> {
        if !Arc::ptr_eq(self.journal(), tracker.journal()) {
            return Err(ClientGitFailure::InvalidResponse);
        }
        GitClient::new(
            self.server().identity(),
            Arc::new(Connection {
                reads: self.clone(),
                tracker,
                peer,
            }),
        )
    }
}
impl GitConnectionPort for Connection {
    fn state(&self) -> Result<GitConnectionState, ClientGitFailure> {
        self.reads.git_state()
    }
    fn query(
        &self,
        query: spaceterm_core::worktrees::GitQuery,
    ) -> GitClientFuture<'_, Result<GitReadResult, ClientGitFailure>> {
        Box::pin(self.reads.git_query(query))
    }
    fn subscribe(
        &self,
        target: GitStatusTarget,
    ) -> Result<Box<dyn GitSubscriptionPort>, ClientGitFailure> {
        self.reads.subscribe_git(target)
    }
    fn submit(
        &self,
        command: Command,
    ) -> GitClientFuture<'_, Result<Arc<dyn GitOperationPort>, ClientGitFailure>> {
        Box::pin(async move {
            self.reads
                .info()
                .map_err(crate::connection::queries::git_failure)?;
            let Command::Worktree(git) = &command else {
                return Err(ClientGitFailure::InvalidResponse);
            };
            let git = git.clone();
            let ticket = self
                .tracker
                .submit(
                    &self.reads.server().binding,
                    command,
                    None,
                    self.peer.as_ref(),
                )
                .await
                .map_err(crate::connection::queries::git_failure)?;
            Ok(Arc::new(Observer {
                reads: self.reads.clone(),
                ticket,
                command: git,
                tracker: self.tracker.clone(),
                peer: self.peer.clone(),
            }) as Arc<dyn GitOperationPort>)
        })
    }
    fn lookup(
        &self,
        id: OperationId,
    ) -> GitClientFuture<'_, Result<Arc<dyn GitOperationPort>, ClientGitFailure>> {
        Box::pin(async move {
            self.reads
                .info()
                .map_err(crate::connection::queries::git_failure)?;
            let state = self
                .tracker
                .journal()
                .snapshot()
                .map_err(crate::connection::queries::git_failure)?;
            let record = state
                .operation(id)
                .ok_or(ClientGitFailure::InvalidResponse)?;
            let request =
                CanonicalOperationRequest::decode(&record.request, self.tracker.journal().codec())
                    .map_err(|_| ClientGitFailure::InvalidResponse)?;
            let Command::Worktree(command) = request.command() else {
                return Err(ClientGitFailure::InvalidResponse);
            };
            let ticket = self
                .tracker
                .ticket(id)
                .map_err(crate::connection::queries::git_failure)?;
            Ok(Arc::new(Observer {
                reads: self.reads.clone(),
                ticket,
                command: command.clone(),
                tracker: self.tracker.clone(),
                peer: self.peer.clone(),
            }) as Arc<dyn GitOperationPort>)
        })
    }
}
struct Observer {
    reads: ServerQueries,
    ticket: OperationTicket,
    command: GitCommand,
    tracker: OperationTracker,
    peer: Arc<dyn OperationPeer>,
}
impl GitOperationPort for Observer {
    fn id(&self) -> OperationId {
        self.ticket.id()
    }
    fn command(&self) -> &GitCommand {
        &self.command
    }
    fn state(&self) -> GitClientFuture<'_, Result<GitOperationState, ClientGitFailure>> {
        Box::pin(async move {
            if matches!(
                *self.ticket.status().borrow(),
                OperationStatus::Submitted | OperationStatus::AwaitingResolution
            ) {
                if self.reads.info().is_err() {
                    return Ok(GitOperationState::Unresolved);
                }
                let lookup = match self.peer.lookup(self.ticket.binding()).await {
                    Ok(lookup) => lookup,
                    Err(_) => return Ok(GitOperationState::Unresolved),
                };
                if let Err(error) = self
                    .tracker
                    .resolve_bound(self.ticket.binding(), lookup, self.peer.as_ref())
                    .await
                {
                    if matches!(
                        error.class(),
                        ClientFailureClass::Unavailable | ClientFailureClass::StaleAuthority
                    ) {
                        return Ok(GitOperationState::Unresolved);
                    }
                    return Err(crate::connection::queries::git_failure(error));
                }
            }
            match self.ticket.status().borrow().clone() {
                OperationStatus::Submitted => Ok(GitOperationState::Pending),
                OperationStatus::AwaitingResolution => Ok(GitOperationState::Unresolved),
                OperationStatus::Retired => Ok(GitOperationState::Retired),
                OperationStatus::Completed {
                    outcome: spaceterm_core::operations::Outcome::Git(outcome),
                    ..
                } => Ok(GitOperationState::Completed(outcome)),
                OperationStatus::Completed {
                    outcome: spaceterm_core::operations::Outcome::Failed(error),
                    ..
                } => Ok(GitOperationState::Rejected(error)),
                _ => Err(crate::connection::queries::git_failure(invalid_peer())),
            }
        })
    }
}
