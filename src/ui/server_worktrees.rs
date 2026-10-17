//! One shared edit writer for the Workspace Window and server Settings.
use crate::{
    local_server::LocalServer,
    terminal::shared_workspace::SharedWorkspaceClient,
    ui::shared_workspaces::{SharedEditSource, SharedSubmission},
};
use gpui::{App, AppContext as _, Global, Task};
use spaceterm_server::{
    hierarchy::{Mutation, Outcome, RetainedOutcome},
    pane::{Reply, Request},
    worktree::{Error, Operation, Read, ReadReply},
};
use std::{path::PathBuf, rc::Rc};

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
    ) -> Task<Result<RetainedOutcome, Error>> {
        let mutation = Mutation::Worktree(operation);
        self.source.snapshot();
        if self.source.submit(mutation.clone()) != SharedSubmission::Submitted {
            return Task::ready(Err(Error::Unavailable));
        }
        let source = self.source.clone();
        cx.spawn(async move |cx| {
            loop {
                if let Some(outcome) = source
                    .snapshot()
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
pub(crate) fn result(outcome: RetainedOutcome) -> Result<(), Error> {
    match outcome.outcome {
        Outcome::Accepted { .. } => Ok(()),
        Outcome::WorktreeFailed(error) => Err(error),
        _ => Err(Error::Unavailable),
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
                return Err(crate::worktrees::git::WorktreeCreateError::Server(
                    checked.err().unwrap_or(Error::Location),
                ));
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
            let outcome = completed.map_err(crate::worktrees::git::WorktreeCreateError::Server)?;
            result(outcome.clone()).map_err(crate::worktrees::git::WorktreeCreateError::Server)?;
            let Outcome::Accepted { target } = outcome.outcome else {
                return Err(crate::worktrees::git::WorktreeCreateError::Failed);
            };
            client
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
                .ok_or(crate::worktrees::git::WorktreeCreateError::Failed)
        })
    }
}

pub(crate) fn available(cx: &App) -> bool {
    cx.try_global::<Client>()
        .is_some_and(|c| c.edits.supports_worktree_writes())
}
