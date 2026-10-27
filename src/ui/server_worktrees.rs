//! One shared edit writer per presented server, for the Workspace Window and server Settings.
use crate::{
    server_connection::ServerConnection,
    terminal::{
        server_session::{ControlRoute, PaneControlRegistry},
        shared_workspace::SharedWorkspaceClient,
    },
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

impl WorktreeReads for ServerConnection {
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
    pub(crate) server: ServerConnection,
    /// Pane control sequencing outlives every window, like this client's connection.
    pub(crate) controls: std::sync::Arc<crate::terminal::server_session::PaneControls>,
    pub(crate) source: SharedEditSource,
    edits: Rc<dyn SharedWorkspaceEdits>,
    reads: Arc<dyn WorktreeReads>,
}
impl Global for Client {}

/// The presented paired servers' writers, by route.
#[derive(Default)]
struct DirectClients(std::collections::HashMap<crate::direct_connections::DirectRouteId, Client>);
impl Global for DirectClients {}

/// Withdrawal drops presentation writers, while original Pane starts retain their controls.
#[derive(Default)]
struct RetainedPaneControls(PaneControlRegistry);
impl Global for RetainedPaneControls {}

impl Client {
    pub(crate) fn get(cx: &mut App) -> Option<Self> {
        if let Some(client) = cx.try_global::<Self>() {
            return Some(client.clone());
        }
        let server = ServerConnection::for_identity(
            crate::application_identity::ApplicationIdentity::current().directory_name(),
        )
        .ok()?;
        let controls = std::sync::Arc::<crate::terminal::server_session::PaneControls>::default();
        let edits = Rc::new(SharedWorkspaceClient::new(
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
        cx.set_global(client.clone());
        Some(client)
    }
    /// The edit writer for a connected paired server, created once per connection so the
    /// Workspace Window and Settings share its sequence and Pane control. A writer kept for an
    /// earlier connection is replaced; its validated original Pane controls remain.
    pub(crate) fn for_direct(
        route: crate::direct_connections::DirectRouteId,
        server: ServerConnection,
        cx: &mut App,
    ) -> Self {
        if let Some(client) = Self::direct(&route, cx)
            .filter(|client| client.server.same_handle(&server))
            .filter(|client| client.controls.admission().is_ok() || server.client_scope().is_err())
        {
            return client;
        }
        let controls = cx
            .default_global::<RetainedPaneControls>()
            .0
            .controls(ControlRoute::Direct(route), server.client_scope());
        let edits = Rc::new(SharedWorkspaceClient::new(
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
        cx.default_global::<DirectClients>()
            .0
            .insert(route, client.clone());
        client
    }
    /// The writer for the server the desktop's direct registry presents for `route`, or the last
    /// one kept for it.
    pub(crate) fn presented_direct(
        route: &crate::direct_connections::DirectRouteId,
        cx: &mut App,
    ) -> Option<Self> {
        match crate::ui::direct_servers::installed(cx).and_then(|direct| direct.server(route)) {
            Some(server) => Some(Self::for_direct(*route, server, cx)),
            None => Self::direct(route, cx),
        }
    }
    /// The edit writer kept for a paired route, if any.
    pub(crate) fn direct(
        route: &crate::direct_connections::DirectRouteId,
        cx: &App,
    ) -> Option<Self> {
        cx.try_global::<DirectClients>()
            .and_then(|clients| clients.0.get(route).cloned())
    }
    /// Drops a withdrawn paired server's writer.
    pub(crate) fn forget_direct(route: &crate::direct_connections::DirectRouteId, cx: &mut App) {
        if cx.has_global::<DirectClients>() {
            cx.global_mut::<DirectClients>().0.remove(route);
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

pub(crate) fn read_server(server: &ServerConnection, read: Read) -> Result<ReadReply, Error> {
    let link = server.link().map_err(|_| Error::Unavailable)?;
    spaceterm_server::worktree::read_worktree(&link, read).map_err(|_| Error::Unavailable)?
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
    /// The other computer whose server runs Git, or `None` for this computer's.
    pub machine: Option<String>,
}
impl crate::ui::worktree_form::WorktreeFormBackend for FormBackend {
    fn server(&self) -> bool {
        true
    }
    fn machine(&self) -> Option<&str> {
        self.machine.as_deref()
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
                    Error::Unavailable => return Err(crate::worktrees::git::WorktreeCreateError::NotSubmitted(if client.availability().0 {"SpaceTerm could not check the Worktree location. Review it before trying again."} else {creation_refusal(SharedRefusalReason::Offline)})),
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

fn creation_refusal(reason: SharedRefusalReason) -> &'static str {
    match reason {
        SharedRefusalReason::Offline => "Reconnect, then create the Worktree again.",
        SharedRefusalReason::Pending => {
            "Wait for the current changes to finish, then create the Worktree again."
        }
        SharedRefusalReason::Value => "Enter a shorter branch name or directory, then try again.",
    }
}

fn create_error(error: WriteError) -> crate::worktrees::git::WorktreeCreateError {
    use crate::worktrees::git::WorktreeCreateError;
    match error {
        WriteError::Refused(reason) => WorktreeCreateError::NotSubmitted(creation_refusal(reason)),
        WriteError::Server(error) => WorktreeCreateError::Server(error),
    }
}

/// Writers whose answers a test scripts, standing in for a server's connection.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use crate::ui::shared_workspaces::testing::ScriptedEdits;
    use spaceterm_server::repository::GitToolStatus;
    use spaceterm_server::worktree::Settings;
    use spaceterm_server::worktree::path_template::{self, WorktreePathTemplateError};
    use std::sync::Mutex;

    /// One server's Server Settings and Git, answered from memory. It records every read it
    /// answers, so a test can tell which server a Worktree read reached.
    pub(crate) struct ScriptedReads {
        settings: Mutex<Settings>,
        reads: Mutex<Vec<Read>>,
        git: Mutex<GitToolStatus>,
        /// Folders the server's account can't write to.
        unwritable: Mutex<Vec<String>>,
    }

    impl ScriptedReads {
        pub(crate) fn new(template: &str) -> Arc<Self> {
            Arc::new(Self {
                settings: Mutex::new(Settings {
                    template: template.to_owned(),
                    version: 0,
                }),
                reads: Mutex::default(),
                git: Mutex::default(),
                unwritable: Mutex::default(),
            })
        }

        /// Every read this server was asked, oldest first.
        pub(crate) fn reads(&self) -> Vec<Read> {
            self.reads.lock().unwrap().clone()
        }

        /// The Git this server reports.
        pub(crate) fn set_git(&self, git: GitToolStatus) {
            *self.git.lock().unwrap() = git;
        }

        /// Makes every location under `folder` one this server can't write to.
        pub(crate) fn refuse_writes_under(&self, folder: &str) {
            self.unwritable.lock().unwrap().push(folder.to_owned());
        }

        /// Another client saves `template`, as a second writer does.
        pub(crate) fn change(&self, template: &str) {
            let mut settings = self.settings.lock().unwrap();
            template.clone_into(&mut settings.template);
            settings.version += 1;
        }
    }

    impl WorktreeReads for ScriptedReads {
        fn read(&self, read: Read) -> Result<ReadReply, Error> {
            self.reads.lock().unwrap().push(read.clone());
            match read {
                Read::Settings => Ok(ReadReply::Settings(self.settings.lock().unwrap().clone())),
                Read::Git => Ok(ReadReply::Git(*self.git.lock().unwrap())),
                Read::ValidateLocationTemplate { template } => {
                    path_template::validate(&template).map_err(Error::Template)?;
                    let unwritable = self.unwritable.lock().unwrap();
                    if unwritable.iter().any(|folder| template.starts_with(folder)) {
                        return Err(Error::Template(WorktreePathTemplateError::NotWritable));
                    }
                    Ok(ReadReply::LocationTemplate)
                }
                Read::Context { .. } => Ok(ReadReply::Context {
                    settings: self.settings.lock().unwrap().clone(),
                    home: "/srv/home".into(),
                    branches: spaceterm_server::worktree::Branches {
                        local: vec!["main".into()],
                        remote: Vec::new(),
                    },
                }),
                Read::Location { location, .. } => Ok(ReadReply::Location(location.into())),
                _ => Err(Error::Unavailable),
            }
        }
    }

    /// Registers this computer's server writer, as the Workspace Window does when it presents
    /// the account-local server.
    pub(crate) fn present_local(edits: Rc<ScriptedEdits>, reads: Arc<ScriptedReads>, cx: &mut App) {
        let client = Client {
            server: ServerConnection::for_identity("spaceterm-development")
                .expect("the development identity resolves"),
            controls: Arc::default(),
            source: SharedEditSource::new(edits.clone()),
            edits,
            reads,
        };
        cx.set_global(client);
    }

    /// Registers a presented paired server's writer, as the Workspace Window does when it
    /// presents that server.
    pub(crate) fn present_direct(
        route: crate::direct_connections::DirectRouteId,
        edits: Rc<ScriptedEdits>,
        reads: Arc<ScriptedReads>,
        cx: &mut App,
    ) {
        // The writer wraps the server the registry presents, as a real presentation's does.
        let server = crate::ui::direct_servers::installed(cx)
            .and_then(|direct| direct.server(&route))
            .unwrap_or_else(|| {
                ServerConnection::for_identity("spaceterm-development")
                    .expect("the development identity resolves")
            });
        let client = Client {
            server,
            controls: Arc::default(),
            source: SharedEditSource::new(edits.clone()),
            edits,
            reads,
        };
        cx.default_global::<DirectClients>().0.insert(route, client);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::shared_workspaces::testing::ScriptedEdits;
    use std::cell::RefCell;

    /// A new connection to the same paired server gets its own writer, so edits never travel
    /// through an earlier connection's.
    #[gpui::test]
    fn a_later_connection_to_one_route_never_reuses_the_earlier_writer(
        cx: &mut gpui::TestAppContext,
    ) {
        let route = [7; 32];
        let first = ServerConnection::for_identity("spaceterm-development").unwrap();
        let later = ServerConnection::for_identity("spaceterm-development").unwrap();
        cx.update(|cx| {
            let original = Client::for_direct(route, first.clone(), cx);
            let again = Client::for_direct(route, first.clone(), cx);
            assert!(
                Arc::ptr_eq(&original.controls, &again.controls),
                "one connection keeps one writer"
            );
            let replaced = Client::for_direct(route, later, cx);
            assert!(
                !Rc::ptr_eq(&original.edits, &replaced.edits),
                "a new connection gets a new presentation writer"
            );
            assert!(Arc::ptr_eq(
                &Client::direct(&route, cx).unwrap().controls,
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
