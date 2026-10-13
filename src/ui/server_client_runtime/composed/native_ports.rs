//! Poll connection-owned futures with the application's retained Tokio runtime.
//! GPUI owns cancellation. No extra executor, transport or operation ledger is created.

use spaceterm_client::{
    api::ServerQueries,
    directories::{ClientDirectoryFailure, DirectoryConnectionPort},
    git::rpc::*,
    journal::JournalController,
};
use spaceterm_core::{
    operation_ledger_contracts::CanonicalOperationRequest,
    operations::{Command, OperationId},
    server_directories::{DirectoryQuery, DirectoryQueryResult},
    worktrees::{GitCommand, GitQuery, GitStatusTarget},
};
use std::{future::Future, sync::Arc};
use tokio::runtime::Handle;

async fn on_runtime<T>(executor: &Handle, future: impl Future<Output = T>) -> T {
    let mut future = std::pin::pin!(future);
    std::future::poll_fn(|cx| {
        let _entered = executor.enter();
        future.as_mut().poll(cx)
    })
    .await
}

pub(super) struct DirectoryPort {
    pub(super) queries: ServerQueries,
    pub(super) executor: Handle,
}
impl DirectoryConnectionPort for DirectoryPort {
    fn state(&self) -> Result<GitConnectionState, ClientDirectoryFailure> {
        DirectoryConnectionPort::state(&self.queries)
    }
    fn query(
        &self,
        query: DirectoryQuery,
    ) -> GitClientFuture<'_, Result<DirectoryQueryResult, ClientDirectoryFailure>> {
        Box::pin(on_runtime(
            &self.executor,
            DirectoryConnectionPort::query(&self.queries, query),
        ))
    }
}

pub(super) struct GitPort {
    pub(super) client: GitClient,
    pub(super) queries: ServerQueries,
    pub(super) journal: Arc<JournalController>,
    pub(super) executor: Handle,
}
impl GitConnectionPort for GitPort {
    fn state(&self) -> Result<GitConnectionState, ClientGitFailure> {
        DirectoryConnectionPort::state(&self.queries).map_err(|error| match error {
            ClientDirectoryFailure::Offline => ClientGitFailure::Offline,
            ClientDirectoryFailure::Reconciling => ClientGitFailure::Reconciling,
            ClientDirectoryFailure::WrongServer => ClientGitFailure::WrongServer,
            ClientDirectoryFailure::StaleResponse => ClientGitFailure::StaleResponse,
            ClientDirectoryFailure::InvalidResponse => ClientGitFailure::InvalidResponse,
            _ => ClientGitFailure::Unavailable,
        })
    }
    fn query(
        &self,
        query: GitQuery,
    ) -> GitClientFuture<'_, Result<GitReadResult, ClientGitFailure>> {
        Box::pin(on_runtime(&self.executor, self.client.query(query)))
    }
    fn subscribe(
        &self,
        target: GitStatusTarget,
    ) -> Result<Box<dyn GitSubscriptionPort>, ClientGitFailure> {
        let _entered = self.executor.enter();
        Ok(Box::new(Subscription {
            inner: self.client.subscribe(target)?,
            executor: self.executor.clone(),
        }))
    }
    fn submit(
        &self,
        command: Command,
    ) -> GitClientFuture<'_, Result<Arc<dyn GitOperationPort>, ClientGitFailure>> {
        Box::pin(async move {
            let Command::Worktree(command) = command else {
                return Err(ClientGitFailure::InvalidResponse);
            };
            let inner = on_runtime(&self.executor, self.client.submit(command.clone())).await?;
            Ok(Arc::new(Operation {
                inner,
                command,
                executor: self.executor.clone(),
            }) as Arc<dyn GitOperationPort>)
        })
    }
    fn lookup(
        &self,
        id: OperationId,
    ) -> GitClientFuture<'_, Result<Arc<dyn GitOperationPort>, ClientGitFailure>> {
        Box::pin(async move {
            let state = self
                .journal
                .snapshot()
                .map_err(|_| ClientGitFailure::Unavailable)?;
            let record = state
                .operation(id)
                .ok_or(ClientGitFailure::InvalidResponse)?;
            let request = CanonicalOperationRequest::decode(&record.request, self.journal.codec())
                .map_err(|_| ClientGitFailure::InvalidResponse)?;
            let Command::Worktree(command) = request.command() else {
                return Err(ClientGitFailure::InvalidResponse);
            };
            let command = command.clone();
            let inner = on_runtime(&self.executor, self.client.resume(id)).await?;
            Ok(Arc::new(Operation {
                inner,
                command,
                executor: self.executor.clone(),
            }) as Arc<dyn GitOperationPort>)
        })
    }
}
struct Subscription {
    inner: GitSubscription,
    executor: Handle,
}
impl GitSubscriptionPort for Subscription {
    fn next(
        &mut self,
    ) -> GitClientFuture<'_, Option<Result<GitSubscriptionEvent, ClientGitFailure>>> {
        Box::pin(on_runtime(&self.executor, self.inner.next()))
    }
}
struct Operation {
    inner: TrackedGitOperation,
    command: GitCommand,
    executor: Handle,
}
impl GitOperationPort for Operation {
    fn id(&self) -> OperationId {
        self.inner.id()
    }
    fn command(&self) -> &GitCommand {
        &self.command
    }
    fn state(&self) -> GitClientFuture<'_, Result<GitOperationState, ClientGitFailure>> {
        Box::pin(on_runtime(&self.executor, self.inner.state()))
    }
}
