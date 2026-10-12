//! Portable Git facade over the connection worker and its existing operation
//! journal. No transport, credentials, retries, or acknowledgements live here.

use spaceterm_core::{
    authority::{ConnectionGeneration, ServerEpoch},
    identity::ServerId,
    operations::{Command, OperationFailure, OperationId},
    server_directories::DirectoryContext,
    worktrees::{queries::*, *},
};
use std::{future::Future, pin::Pin, sync::Arc};

pub type GitClientFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ClientGitFailure {
    #[error("server is offline")]
    Offline,
    #[error("server state is not reconciled")]
    Reconciling,
    #[error("Git target belongs to another server")]
    WrongServer,
    #[error("Git assessment belongs to another principal")]
    Unauthorized,
    #[error("stale Git response")]
    StaleResponse,
    #[error("Git resynchronization required")]
    ResyncRequired,
    #[error("Git resource capacity exhausted")]
    Capacity,
    #[error("Git connection provider unavailable")]
    Unavailable,
    #[error("invalid Git response")]
    InvalidResponse,
    #[error("Git request rejected ({0})")]
    Remote(GitFailure),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitReadiness {
    Ready,
    Reconciling,
    Offline,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GitConnectionState {
    pub context: DirectoryContext,
    pub generation: ConnectionGeneration,
    pub readiness: GitReadiness,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitReadResult {
    pub epoch: ServerEpoch,
    pub scope: Option<spaceterm_core::operations::RepositoryScope>,
    pub result: GitQueryResult,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GitSubscriptionEvent {
    Status(Box<GitReadResult>),
    /// Retain the previous successful facts. Never synthesize an empty inventory.
    LastKnown(GitFailure),
    ResyncRequired,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GitOperationState {
    Pending,
    /// Admission may have succeeded; resolve only the original identity.
    Unresolved,
    Completed(GitMutationOutcome),
    Rejected(OperationFailure),
    Retired,
}

/// Implemented by the per-server client-core worker. It owns read correlation,
/// authenticated capabilities, atomic online admission, canonical journaling,
/// outcome retention and durable acknowledgement. A test fake is not production
/// composition. This port cannot perform local filesystem or GitHub actions.
pub trait GitConnectionPort: Send + Sync {
    fn state(&self) -> Result<GitConnectionState, ClientGitFailure>;
    fn query(
        &self,
        query: GitQuery,
    ) -> GitClientFuture<'_, Result<GitReadResult, ClientGitFailure>>;
    fn subscribe(
        &self,
        target: GitStatusTarget,
    ) -> Result<Box<dyn GitSubscriptionPort>, ClientGitFailure>;
    fn submit(
        &self,
        command: Command,
    ) -> GitClientFuture<'_, Result<Arc<dyn GitOperationPort>, ClientGitFailure>>;
    fn lookup(
        &self,
        operation: OperationId,
    ) -> GitClientFuture<'_, Result<Arc<dyn GitOperationPort>, ClientGitFailure>>;
}
/// Dropping this read-side receiver cancels only its connection-owned interest.
pub trait GitSubscriptionPort: Send {
    fn next(
        &mut self,
    ) -> GitClientFuture<'_, Option<Result<GitSubscriptionEvent, ClientGitFailure>>>;
}
/// Borrowed access to the existing operation tracker. No second ledger or retry
/// API. Once admitted, dropping every observer cannot cancel the server write.
pub trait GitOperationPort: Send + Sync {
    fn id(&self) -> OperationId;
    fn command(&self) -> &GitCommand;
    fn state(&self) -> GitClientFuture<'_, Result<GitOperationState, ClientGitFailure>>;
}

#[derive(Clone)]
pub struct GitClient {
    server: ServerId,
    connection: Arc<dyn GitConnectionPort>,
}
impl GitClient {
    pub fn new(
        server: ServerId,
        connection: Arc<dyn GitConnectionPort>,
    ) -> Result<Self, ClientGitFailure> {
        let state = connection.state()?;
        if state.context.server() != server {
            return Err(ClientGitFailure::WrongServer);
        }
        Ok(Self { server, connection })
    }
    pub fn server(&self) -> ServerId {
        self.server
    }
    pub async fn query(&self, query: GitQuery) -> Result<GitReadResult, ClientGitFailure> {
        query
            .validate_scope(self.server)
            .map_err(ClientGitFailure::Remote)?;
        let state = self.ready()?;
        if query_cursor(&query).is_some_and(|c| c.parts().epoch != state.context.parts().epoch) {
            return Err(ClientGitFailure::Remote(GitFailure::StaleAssessment));
        }
        if let GitQuery::AssessmentPage { assessment, .. } = &query {
            validate_token(assessment, state.context, true)?;
        }
        let result = self.connection.query(query.clone()).await?;
        if self.connection.state()? != state {
            return Err(ClientGitFailure::StaleResponse);
        }
        validate_read(&query, &result, state.context)?;
        Ok(result)
    }
    pub fn subscribe(&self, target: GitStatusTarget) -> Result<GitSubscription, ClientGitFailure> {
        if target.server() != self.server {
            return Err(ClientGitFailure::WrongServer);
        }
        let state = self.ready()?;
        let receiver = self.connection.subscribe(target)?;
        Ok(GitSubscription {
            target,
            state,
            connection: self.connection.clone(),
            receiver,
            closed: false,
        })
    }
    pub async fn submit(
        &self,
        command: GitCommand,
    ) -> Result<TrackedGitOperation, ClientGitFailure> {
        let state = self.ready()?;
        validate_token(command.assessment(), state.context, true)?;
        let expected = match &command {
            GitCommand::CreateConfirmed { .. } => GitAssessmentKind::Create,
            GitCommand::RemoveConfirmed { .. } => GitAssessmentKind::Remove,
        };
        if command.assessment().kind() != expected {
            return Err(ClientGitFailure::Remote(GitFailure::StaleAssessment));
        }
        let tracker = self
            .connection
            .submit(Command::Worktree(command.clone()))
            .await?;
        // A disconnect after admission must not turn the original operation into
        // a definite failure or a replacement submission.
        validate_tracker(tracker.as_ref(), state.context, Some(&command))?;
        Ok(TrackedGitOperation {
            tracker,
            server: self.server,
        })
    }
    pub async fn resume(
        &self,
        operation: OperationId,
    ) -> Result<TrackedGitOperation, ClientGitFailure> {
        let state = self.ready()?;
        let p = state.context.parts();
        if operation.stream().server() != self.server {
            return Err(ClientGitFailure::WrongServer);
        }
        if operation.stream().pairing() != p.pairing || operation.stream().client() != p.client {
            return Err(ClientGitFailure::Unauthorized);
        }
        let tracker = self.connection.lookup(operation).await?;
        if tracker.id() != operation {
            return Err(ClientGitFailure::InvalidResponse);
        }
        // Historical assessments may belong to a previous epoch. Lookup is not
        // a new confirmed write and must not reject a retained original result.
        validate_tracker(tracker.as_ref(), state.context, None)?;
        Ok(TrackedGitOperation {
            tracker,
            server: self.server,
        })
    }
    fn ready(&self) -> Result<GitConnectionState, ClientGitFailure> {
        let state = self.connection.state()?;
        if state.context.server() != self.server {
            return Err(ClientGitFailure::WrongServer);
        }
        if state.context.parts().epoch.get() == 0 || state.generation.get() == 0 {
            return Err(ClientGitFailure::InvalidResponse);
        }
        match state.readiness {
            GitReadiness::Ready => Ok(state),
            GitReadiness::Offline => Err(ClientGitFailure::Offline),
            GitReadiness::Reconciling => Err(ClientGitFailure::Reconciling),
        }
    }
}

pub struct GitSubscription {
    target: GitStatusTarget,
    state: GitConnectionState,
    connection: Arc<dyn GitConnectionPort>,
    receiver: Box<dyn GitSubscriptionPort>,
    closed: bool,
}
impl GitSubscription {
    pub fn target(&self) -> GitStatusTarget {
        self.target
    }
    pub async fn next(&mut self) -> Option<Result<GitSubscriptionEvent, ClientGitFailure>> {
        if self.closed {
            return None;
        }
        let event = self.receiver.next().await?;
        let result = (|| {
            if self.connection.state()? != self.state {
                return Err(ClientGitFailure::StaleResponse);
            }
            if let GitSubscriptionEvent::Status(read) = &event.as_ref().map_err(|e| *e)? {
                validate_read(
                    &GitQuery::Status {
                        target: self.target,
                    },
                    read,
                    self.state.context,
                )?;
            }
            event
        })();
        if result.is_err() {
            self.closed = true;
        }
        Some(result)
    }
}

#[derive(Clone)]
pub struct TrackedGitOperation {
    tracker: Arc<dyn GitOperationPort>,
    server: ServerId,
}
impl TrackedGitOperation {
    pub fn id(&self) -> OperationId {
        self.tracker.id()
    }
    pub async fn state(&self) -> Result<GitOperationState, ClientGitFailure> {
        let state = self.tracker.state().await?;
        if let GitOperationState::Completed(outcome) = &state {
            outcome
                .validate_scope(self.server)
                .map_err(ClientGitFailure::Remote)?;
        }
        Ok(state)
    }
}
fn validate_tracker(
    tracker: &dyn GitOperationPort,
    context: DirectoryContext,
    command: Option<&GitCommand>,
) -> Result<(), ClientGitFailure> {
    let p = context.parts();
    let id = tracker.id();
    if id.stream().server() != p.server {
        return Err(ClientGitFailure::WrongServer);
    }
    if id.stream().pairing() != p.pairing || id.stream().client() != p.client {
        return Err(ClientGitFailure::Unauthorized);
    }
    if command.is_some_and(|c| c != tracker.command()) {
        return Err(ClientGitFailure::InvalidResponse);
    }
    tracker
        .command()
        .validate_stream(id.stream())
        .map_err(ClientGitFailure::Remote)?;
    validate_token(tracker.command().assessment(), context, false)
}
fn validate_token(
    token: &GitAssessmentToken,
    context: DirectoryContext,
    current_epoch: bool,
) -> Result<(), ClientGitFailure> {
    let p = context.parts();
    if token.server() != p.server {
        return Err(ClientGitFailure::WrongServer);
    }
    if token.parts().pairing != p.pairing || token.parts().client != p.client {
        return Err(ClientGitFailure::Unauthorized);
    }
    if current_epoch && token.epoch() != p.epoch {
        return Err(ClientGitFailure::Remote(GitFailure::StaleAssessment));
    }
    Ok(())
}
fn validate_read(
    query: &GitQuery,
    read: &GitReadResult,
    context: DirectoryContext,
) -> Result<(), ClientGitFailure> {
    if read.epoch != context.parts().epoch {
        return Err(ClientGitFailure::StaleResponse);
    }
    read.result
        .validate(read.scope, context.server())
        .map_err(ClientGitFailure::Remote)?;
    let next = match &read.result {
        GitQueryResult::Inventory(page) => page.next.as_ref(),
        GitQueryResult::Branches(page) => page.next.as_ref(),
        GitQueryResult::Assessment(page) => page.next.as_ref(),
        GitQueryResult::Status(_) => None,
    };
    if next.is_some_and(|c| c.parts().epoch != read.epoch) {
        return Err(ClientGitFailure::ResyncRequired);
    }
    if let Some(cursor) = query_cursor(query)
        && (read.scope != Some(cursor.parts().scope)
            || next.is_some_and(|c| {
                c.parts().observation != cursor.parts().observation || c == cursor
            })
            || matches!(&read.result, GitQueryResult::Inventory(page) if page.version.get() != cursor.parts().observation))
    {
        return Err(ClientGitFailure::ResyncRequired);
    }
    match (query, &read.result) {
        (GitQuery::Status { .. }, GitQueryResult::Status(_)) => {}
        (GitQuery::Inventory { .. }, GitQueryResult::Inventory(_)) => {}
        (GitQuery::Branches { repository, .. }, GitQueryResult::Branches(_))
            if read.scope == Some(*repository) => {}
        (GitQuery::PreviewCreate { request }, GitQueryResult::Assessment(page))
            if page.token.kind() == GitAssessmentKind::Create
                && page
                    .creation_preview
                    .as_ref()
                    .is_some_and(|p| &p.parts().draft == request) =>
        {
            validate_token(&page.token, context, true)?
        }
        (GitQuery::AssessRemove { worktree }, GitQueryResult::Assessment(page))
            if page.token.kind() == GitAssessmentKind::Remove
                && page
                    .removal
                    .as_ref()
                    .is_some_and(|summary| summary.worktree == *worktree) =>
        {
            validate_token(&page.token, context, true)?
        }
        (GitQuery::AssessmentPage { assessment, .. }, GitQueryResult::Assessment(page))
            if assessment == &page.token
                && (assessment.kind() != GitAssessmentKind::Remove || page.removal.is_some()) =>
        {
            validate_token(&page.token, context, true)?
        }
        _ => return Err(ClientGitFailure::InvalidResponse),
    }
    Ok(())
}

fn query_cursor(query: &GitQuery) -> Option<&GitPageCursor> {
    match query {
        GitQuery::Inventory { cursor, .. }
        | GitQuery::Branches { cursor, .. }
        | GitQuery::AssessmentPage { cursor, .. } => cursor.as_ref(),
        _ => None,
    }
}
