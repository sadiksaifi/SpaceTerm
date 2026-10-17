//! One shared edit writer for the Workspace Window and server Settings.
use crate::{
    local_server::LocalServer,
    terminal::shared_workspace::SharedWorkspaceClient,
    ui::shared_workspaces::{SharedEditSource, SharedRefusalReason, SharedSubmission},
};
use gpui::{App, AppContext as _, Global, Task};
use spaceterm_server::{
    hierarchy::{Mutation, Outcome, RetainedOutcome},
    pane::{Reply, Request},
    worktree::{Error, Operation, Read, ReadReply},
};
use std::{path::PathBuf, rc::Rc};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WriteError {
    Refused(SharedRefusalReason),
    Server(Error),
}
impl From<Error> for WriteError {
    fn from(error: Error) -> Self {
        Self::Server(error)
    }
}

#[derive(Clone)]
pub(crate) struct Client {
    pub(crate) server: LocalServer,
    pub(crate) source: SharedEditSource,
    edits: Rc<SharedWorkspaceClient>,
}
impl Global for Client {}
impl Client {
    pub(crate) fn get(cx: &mut App) -> Option<Self> {
        if let Some(client) = cx.try_global::<Self>() {
            return Some(client.clone());
        }
        let server = LocalServer::for_identity(
            crate::application_identity::ApplicationIdentity::current().directory_name(),
        )
        .ok()?;
        let legacy = crate::ui::worktree_store::InstalledWorktrees::store(cx)
            .map(|s| s.read(cx).path_template())
            .unwrap_or_else(|| spaceterm_server::worktree::Settings::default().template);
        let edits = Rc::new(SharedWorkspaceClient::new(server.clone(), legacy));
        let client = Self {
            server,
            source: SharedEditSource::new(edits.clone()),
            edits,
        };
        cx.set_global(client.clone());
        Some(client)
    }
    pub(crate) fn supports_worktree_writes(&self) -> bool {
        self.edits.supports_worktree_writes()
    }
    pub(crate) fn availability(&self) -> (bool, bool) {
        self.edits.availability()
    }
    pub(crate) fn read(&self, read: Read, cx: &mut App) -> Task<Result<ReadReply, Error>> {
        let server = self.server.clone();
        cx.background_spawn(async move { read_server(&server, read) })
    }
    pub(crate) fn write(
        &self,
        operation: Operation,
        cx: &mut App,
    ) -> Task<Result<RetainedOutcome, WriteError>> {
        let mutation = Mutation::Worktree(operation);
        self.source.snapshot();
        if let SharedSubmission::Refused(reason) = self.source.submit(mutation.clone()) {
            return Task::ready(Err(WriteError::Refused(reason)));
        }
        let source = self.source.clone();
        cx.spawn(async move |cx| {
            loop {
                let snapshot = source.snapshot();
                if snapshot.rejected.as_ref() == Some(&mutation) {
                    return Err(WriteError::Refused(SharedRefusalReason::Offline));
                }
                if let Some(outcome) = snapshot
                    .outcome
                    .filter(|outcome| outcome.mutation == mutation)
                {
                    return Ok(outcome);
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(50))
                    .await;
            }
        })
    }
}
pub(crate) fn read_server(server: &LocalServer, read: Read) -> Result<ReadReply, Error> {
    match server
        .connect_shared_pane()
        .and_then(|mut c| c.exchange(&Request::WorktreeRead(read)))
    {
        Ok(Reply::WorktreeRead(reply)) => reply,
        _ => Err(Error::Unavailable),
    }
}
pub(crate) fn result(outcome: RetainedOutcome) -> Result<(), WriteError> {
    match outcome.outcome {
        Outcome::Accepted { .. } => Ok(()),
        Outcome::WorktreeFailed(error) => Err(error.into()),
        _ => Err(Error::Unavailable.into()),
    }
}
pub(crate) struct FormBackend {
    pub client: Client,
    pub root: PathBuf,
    pub workspace: [u8; 32],
    pub branches: crate::worktrees::git::BranchList,
}
impl crate::ui::worktree_form::WorktreeFormBackend for FormBackend {
    fn server(&self) -> bool {
        true
    }
    fn connected(&self) -> bool {
        self.client.availability().0
    }
    fn branches(&self, _: &mut App) -> Task<Option<crate::worktrees::git::BranchList>> {
        Task::ready(Some(self.branches.clone()))
    }
    fn create(
        &self,
        path: PathBuf,
        branch: crate::worktrees::git::WorktreeBranch,
        cx: &mut App,
    ) -> Task<Result<PathBuf, crate::worktrees::git::WorktreeCreateError>> {
        let client = self.client.clone();
        let root = self.root.clone();
        let workspace = self.workspace;
        // The owning server checks the final location; the desktop never probes its filesystem.
        let read = client.read(
            Read::Location {
                root: root.clone(),
                branch: branch.name().into(),
                location: Some(path.display().to_string()),
            },
            cx,
        );
        cx.spawn(async move |cx| {
            let checked = read.await;
            let Ok(ReadReply::Location(path)) = checked else {
                return Err(create_error(match checked.err().unwrap_or(Error::Location) {
                    Error::Unavailable => return Err(crate::worktrees::git::WorktreeCreateError::NotSubmitted(if client.availability().0 {"SpaceTerm couldn\u{2019}t validate the location on the server. Review the location before trying again."} else {SharedRefusalReason::Offline.explanation()})),
                    error => error.into(),
                }));
            };
            let completed = cx
                .update(|cx| {
                    client.write(
                        Operation::Create {
                            workspace,
                            root,
                            path: path.clone(),
                            branch,
                        },
                        cx,
                    )
                })
                .await;
            let outcome = completed.map_err(create_error)?;
            result(outcome.clone()).map_err(create_error)?;
            let Outcome::Accepted { target } = outcome.outcome else {
                return Err(crate::worktrees::git::WorktreeCreateError::Failed);
            };
            Ok(client
                .source
                .snapshot()
                .inventory
                .and_then(|i| {
                    i.workspaces
                        .into_iter()
                        .flat_map(|w| w.tabs)
                        .find(|t| t.id == target)
                        .and_then(|t| t.worktree)
                })
                .unwrap_or(path))
        })
    }
}

pub(crate) fn available(cx: &App) -> bool {
    cx.try_global::<Client>()
        .is_some_and(|c| c.edits.supports_worktree_writes())
}

fn create_error(error: WriteError) -> crate::worktrees::git::WorktreeCreateError {
    use crate::worktrees::git::WorktreeCreateError;
    match error {
        WriteError::Refused(reason) => WorktreeCreateError::NotSubmitted(reason.explanation()),
        WriteError::Server(error) => WorktreeCreateError::Server(error),
    }
}
