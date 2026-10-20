//! One shared edit writer per presented server, for the Workspace Window and server Settings.
use crate::{
    local_server::LocalServer,
    terminal::shared_workspace::SharedWorkspaceClient,
    ui::shared_workspaces::{SharedEditSource, SharedRefusalReason, SharedWorkspaceEdits},
};
use gpui::{App, AppContext as _, Global, Task};
use spaceterm_server::{
    hierarchy::{Mutation, Outcome, RetainedOutcome},
    worktree::{Error, Operation, Read, ReadReply},
};
use std::{path::PathBuf, rc::Rc, sync::Arc};

/// Answers one server's Worktree reads over its own connection. Reading blocks, so callers run
/// it on a worker.
pub(crate) trait WorktreeReads: Send + Sync {
    fn read(&self, read: Read) -> Result<ReadReply, Error>;
}

impl WorktreeReads for LocalServer {
    fn read(&self, read: Read) -> Result<ReadReply, Error> {
        read_server(self, read)
    }
}

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
    /// Pane control sequencing outlives every window, like this client's connection.
    pub(crate) controls: std::sync::Arc<crate::terminal::server_session::PaneControls>,
    pub(crate) source: SharedEditSource,
    edits: Rc<dyn SharedWorkspaceEdits>,
    reads: Arc<dyn WorktreeReads>,
}
impl Global for Client {}

/// The presented SSH servers' writers, by SSH destination.
#[derive(Default)]
struct SshClients(std::collections::HashMap<String, Client>);
impl Global for SshClients {}
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
        let controls = std::sync::Arc::<crate::terminal::server_session::PaneControls>::default();
        let edits = Rc::new(SharedWorkspaceClient::new(
            server.clone(),
            std::sync::Arc::clone(&controls),
            legacy,
        ));
        let client = Self {
            reads: Arc::new(server.clone()),
            server,
            controls,
            source: SharedEditSource::new(edits.clone()),
            edits,
        };
        cx.set_global(client.clone());
        Some(client)
    }
    /// The edit writer for a connected SSH server's account server, created once per connection
    /// so the Workspace Window and Settings share its sequence and Pane control. A writer kept
    /// for an earlier connection to that destination is replaced, never reused.
    pub(crate) fn for_ssh(
        destination: &crate::domain::SshDestination,
        server: LocalServer,
        cx: &mut App,
    ) -> Self {
        if let Some(client) =
            Self::ssh(destination, cx).filter(|client| client.server.same_handle(&server))
        {
            return client;
        }
        let controls = std::sync::Arc::<crate::terminal::server_session::PaneControls>::default();
        let edits = Rc::new(SharedWorkspaceClient::remote(
            server.clone(),
            std::sync::Arc::clone(&controls),
        ));
        let client = Self {
            reads: Arc::new(server.clone()),
            server,
            controls,
            source: SharedEditSource::new(edits.clone()),
            edits,
        };
        cx.default_global::<SshClients>()
            .0
            .insert(destination.as_str().to_owned(), client.clone());
        client
    }
    /// The writer for the server the desktop's SSH registry presents for `destination`, even
    /// while no Workspace Window presents it. Without a presented server it is the last writer
    /// kept for that destination, whose reads fail once its connection is gone.
    pub(crate) fn presented_ssh(
        destination: &crate::domain::SshDestination,
        cx: &mut App,
    ) -> Option<Self> {
        match crate::ui::ssh_servers::installed(cx).and_then(|ssh| ssh.server(destination)) {
            Some(server) => Some(Self::for_ssh(destination, server, cx)),
            None => Self::ssh(destination, cx),
        }
    }
    /// The edit writer kept for an SSH destination, if any.
    pub(crate) fn ssh(destination: &crate::domain::SshDestination, cx: &App) -> Option<Self> {
        cx.try_global::<SshClients>()
            .and_then(|clients| clients.0.get(destination.as_str()).cloned())
    }
    /// Drops a withdrawn SSH server's writer. A later connection starts a new one.
    pub(crate) fn forget_ssh(destination: &crate::domain::SshDestination, cx: &mut App) {
        if cx.has_global::<SshClients>() {
            cx.global_mut::<SshClients>().0.remove(destination.as_str());
        }
    }
    pub(crate) fn supports_worktree_writes(&self) -> bool {
        self.edits.worktree_writes()
    }
    pub(crate) fn availability(&self) -> (bool, bool) {
        self.edits.availability()
    }
    pub(crate) fn read(&self, read: Read, cx: &mut App) -> Task<Result<ReadReply, Error>> {
        let reads = Arc::clone(&self.reads);
        cx.background_spawn(async move { reads.read(read) })
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
        .is_some_and(Client::supports_worktree_writes)
}

fn create_error(error: WriteError) -> crate::worktrees::git::WorktreeCreateError {
    use crate::worktrees::git::WorktreeCreateError;
    match error {
        WriteError::Refused(reason) => WorktreeCreateError::NotSubmitted(reason.explanation()),
        WriteError::Server(error) => WorktreeCreateError::Server(error),
    }
}

/// Writers whose answers a test scripts, standing in for a server's connection.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use crate::ui::shared_workspaces::testing::ScriptedEdits;
    use spaceterm_server::worktree::Settings;
    use std::sync::Mutex;

    /// One server's Server Settings, answered from memory.
    pub(crate) struct ScriptedReads(Mutex<Settings>);

    impl ScriptedReads {
        pub(crate) fn new(template: &str) -> Arc<Self> {
            Arc::new(Self(Mutex::new(Settings {
                template: template.to_owned(),
                version: 0,
            })))
        }
    }

    impl WorktreeReads for ScriptedReads {
        fn read(&self, read: Read) -> Result<ReadReply, Error> {
            match read {
                Read::Settings => Ok(ReadReply::Settings(self.0.lock().unwrap().clone())),
                _ => Err(Error::Unavailable),
            }
        }
    }

    /// Registers a presented SSH server's writer, as the Workspace Window does when it presents
    /// that server.
    pub(crate) fn present_ssh(
        destination: &crate::domain::SshDestination,
        edits: Rc<ScriptedEdits>,
        reads: Arc<ScriptedReads>,
        cx: &mut App,
    ) {
        // The writer wraps the server the registry presents, as a real presentation's does.
        let server = crate::ui::ssh_servers::installed(cx)
            .and_then(|ssh| ssh.server(destination))
            .unwrap_or_else(|| {
                LocalServer::for_identity("spaceterm-development")
                    .expect("the development identity resolves")
            });
        let client = Client {
            server,
            controls: Arc::default(),
            source: SharedEditSource::new(edits.clone()),
            edits,
            reads,
        };
        cx.default_global::<SshClients>()
            .0
            .insert(destination.as_str().to_owned(), client);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::shared_workspaces::testing::ScriptedEdits;
    use std::cell::RefCell;

    /// A new connection to the same destination gets its own writer, so edits never travel
    /// through an earlier connection's.
    #[gpui::test]
    fn a_later_connection_to_one_destination_never_reuses_the_earlier_writer(
        cx: &mut gpui::TestAppContext,
    ) {
        let destination = crate::domain::SshDestination::new("penguin".to_owned()).unwrap();
        let first = LocalServer::for_identity("spaceterm-development").unwrap();
        let later = LocalServer::for_identity("spaceterm-development").unwrap();
        cx.update(|cx| {
            let original = Client::for_ssh(&destination, first.clone(), cx);
            let again = Client::for_ssh(&destination, first.clone(), cx);
            assert!(
                Arc::ptr_eq(&original.controls, &again.controls),
                "one connection keeps one writer"
            );
            let replaced = Client::for_ssh(&destination, later, cx);
            assert!(!Arc::ptr_eq(&original.controls, &replaced.controls));
            assert!(Arc::ptr_eq(
                &Client::ssh(&destination, cx).unwrap().controls,
                &replaced.controls
            ));
        });
    }

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
