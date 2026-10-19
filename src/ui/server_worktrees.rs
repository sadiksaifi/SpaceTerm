//! One shared edit writer for the Workspace Window and server Settings.
use crate::{
    local_server::LocalServer,
    terminal::shared_workspace::SharedWorkspaceClient,
    ui::shared_workspaces::{SharedEditSource, SharedRefusalReason},
};
use gpui::{App, AppContext as _, Global, Task};
use spaceterm_server::{
    hierarchy::{Mutation, Outcome, RetainedOutcome},
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
        let legacy = crate::ui::worktree_path_template::path_template(cx)
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
        write(self.source.clone(), operation, cx)
    }
}
fn write(
    source: SharedEditSource,
    operation: Operation,
    cx: &mut App,
) -> Task<Result<RetainedOutcome, WriteError>> {
    source.snapshot();
    let completion = match source.submit_waiting(Mutation::Worktree(operation)) {
        Ok(completion) => completion,
        Err(reason) => return Task::ready(Err(WriteError::Refused(reason))),
    };
    cx.spawn(async move |cx| {
        loop {
            source.snapshot();
            match completion.try_recv() {
                Ok(result) => return result.map_err(WriteError::Refused),
                Err(async_channel::TryRecvError::Closed) => return Err(Error::Unavailable.into()),
                Err(async_channel::TryRecvError::Empty) => (),
            }
            cx.background_executor()
                .timer(std::time::Duration::from_millis(50))
                .await;
        }
    })
}

pub(crate) fn read_server(server: &LocalServer, read: Read) -> Result<ReadReply, Error> {
    let mut connection = server
        .connect_shared_pane()
        .map_err(|_| Error::Unavailable)?;
    if !connection.worktree_writes() {
        return Err(Error::Unavailable);
    }
    connection
        .read_worktree(read)
        .map_err(|_| Error::Unavailable)?
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
                location: path.display().to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::shared_workspaces::testing::ScriptedEdits;
    use std::cell::RefCell;

    #[gpui::test]
    fn shared_write_waiters_keep_their_completions_when_later_edits_finish(
        cx: &mut gpui::TestAppContext,
    ) {
        let edits = Rc::new(ScriptedEdits::new(None));
        let source = SharedEditSource::new(edits.clone());
        let replies = Rc::new(RefCell::new(Vec::new()));
        let first = Operation::SetLocation {
            version: 0,
            template: "~/first/{branch}".into(),
        };
        let second = Operation::SetLocation {
            version: 1,
            template: "~/second/{branch}".into(),
        };
        let first_task = cx.update(|cx| write(source.clone(), first.clone(), cx));
        let recorded = replies.clone();
        cx.spawn(async move |_| {
            let reply = first_task.await;
            recorded.borrow_mut().push(reply);
        })
        .detach();
        cx.run_until_parked();
        edits.answer(Outcome::Accepted { target: [1; 32] });
        // The Workspace poll sees A before its sleeping waiter does. B may now submit.
        source.snapshot();
        let second_task = cx.update(|cx| write(source.clone(), second.clone(), cx));
        let recorded = replies.clone();
        cx.spawn(async move |_| {
            let reply = second_task.await;
            recorded.borrow_mut().push(reply);
        })
        .detach();
        cx.run_until_parked();
        edits.answer(Outcome::Accepted { target: [1; 32] });
        source.snapshot();
        cx.background_executor
            .advance_clock(std::time::Duration::from_millis(100));
        cx.run_until_parked();
        let mut received: Vec<_> = replies
            .borrow()
            .iter()
            .map(|reply| {
                let reply = reply.as_ref().expect("each submission gets its own answer");
                (reply.sequence, reply.mutation.clone())
            })
            .collect();
        received.sort_by_key(|(sequence, _)| *sequence);
        assert_eq!(
            received,
            vec![
                (1, Mutation::Worktree(first)),
                (2, Mutation::Worktree(second))
            ]
        );
    }
}
