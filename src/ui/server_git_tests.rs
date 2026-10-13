use super::*;
use gpui::TestAppContext;
use spaceterm_client::git::rpc::*;
use spaceterm_core::{
    authority::{ConnectionGeneration, FieldVersion, OrganizationRevision},
    identity::*,
    operations::{Command, OperationId},
    organization::ServerDirectory,
    server_directories::{DirectoryContext, DirectoryContextParts},
    worktrees::{queries::*, *},
};
use std::{
    collections::VecDeque,
    num::NonZeroU64,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

struct Peer {
    state: Mutex<GitConnectionState>,
    reads: Mutex<VecDeque<GitReadResult>>,
    events: async_channel::Receiver<Result<GitSubscriptionEvent, ClientGitFailure>>,
    queries: AtomicUsize,
    drops: Arc<AtomicUsize>,
    submissions: AtomicUsize,
    operation: Mutex<Option<Arc<Operation>>>,
    lookups: Mutex<Vec<OperationId>>,
    delayed: Mutex<Option<async_channel::Receiver<GitReadResult>>>,
    requests: Mutex<Vec<GitQuery>>,
}
struct Operation {
    id: OperationId,
    command: GitCommand,
    state: Mutex<GitOperationState>,
}
impl GitOperationPort for Operation {
    fn id(&self) -> OperationId {
        self.id
    }
    fn command(&self) -> &GitCommand {
        &self.command
    }
    fn state(&self) -> GitClientFuture<'_, Result<GitOperationState, ClientGitFailure>> {
        let state = self.state.lock().unwrap().clone();
        Box::pin(async move { Ok(state) })
    }
}
struct Receiver {
    events: async_channel::Receiver<Result<GitSubscriptionEvent, ClientGitFailure>>,
    drops: Arc<AtomicUsize>,
}
impl Drop for Receiver {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}
impl GitSubscriptionPort for Receiver {
    fn next(
        &mut self,
    ) -> GitClientFuture<'_, Option<Result<GitSubscriptionEvent, ClientGitFailure>>> {
        Box::pin(async move { self.events.recv().await.ok() })
    }
}
impl GitConnectionPort for Peer {
    fn state(&self) -> Result<GitConnectionState, ClientGitFailure> {
        Ok(*self.state.lock().unwrap())
    }
    fn query(
        &self,
        query: GitQuery,
    ) -> GitClientFuture<'_, Result<GitReadResult, ClientGitFailure>> {
        self.queries.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(query);
        if let Some(receiver) = self.delayed.lock().unwrap().take() {
            return Box::pin(async move {
                receiver
                    .recv()
                    .await
                    .map_err(|_| ClientGitFailure::Unavailable)
            });
        }
        let read = self
            .reads
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(ClientGitFailure::Unavailable);
        Box::pin(async move { read })
    }
    fn subscribe(
        &self,
        _: GitStatusTarget,
    ) -> Result<Box<dyn GitSubscriptionPort>, ClientGitFailure> {
        Ok(Box::new(Receiver {
            events: self.events.clone(),
            drops: self.drops.clone(),
        }))
    }
    fn submit(
        &self,
        command: Command,
    ) -> GitClientFuture<'_, Result<Arc<dyn GitOperationPort>, ClientGitFailure>> {
        self.submissions.fetch_add(1, Ordering::SeqCst);
        let operation = self.operation.lock().unwrap().clone();
        if let Some(operation) = &operation {
            assert_eq!(command, Command::Worktree(operation.command.clone()));
        }
        Box::pin(async move {
            operation
                .map(|operation| operation as Arc<dyn GitOperationPort>)
                .ok_or(ClientGitFailure::Unavailable)
        })
    }
    fn lookup(
        &self,
        id: OperationId,
    ) -> GitClientFuture<'_, Result<Arc<dyn GitOperationPort>, ClientGitFailure>> {
        self.lookups.lock().unwrap().push(id);
        let operation = self.operation.lock().unwrap().clone();
        Box::pin(async move {
            operation
                .map(|operation| operation as Arc<dyn GitOperationPort>)
                .ok_or(ClientGitFailure::Unavailable)
        })
    }
}
fn peer(
    server: ServerId,
) -> (
    Arc<Peer>,
    async_channel::Sender<Result<GitSubscriptionEvent, ClientGitFailure>>,
) {
    let (send, events) = async_channel::bounded(16);
    (
        Arc::new(Peer {
            state: Mutex::new(GitConnectionState {
                context: DirectoryContext::new(DirectoryContextParts {
                    server,
                    epoch: ServerEpoch::new(7),
                    pairing: PairingId::new(server, 3).unwrap(),
                    client: ClientId::new(server, 4).unwrap(),
                })
                .unwrap(),
                generation: ConnectionGeneration::new(1),
                readiness: GitReadiness::Ready,
            }),
            reads: Mutex::new(VecDeque::new()),
            events,
            queries: AtomicUsize::new(0),
            drops: Arc::new(AtomicUsize::new(0)),
            submissions: AtomicUsize::new(0),
            operation: Mutex::new(None),
            lookups: Mutex::new(Vec::new()),
            delayed: Mutex::new(None),
            requests: Mutex::new(Vec::new()),
        }),
        send,
    )
}
fn status(server: ServerId, revision: u64, dirty: bool) -> GitReadResult {
    GitReadResult {
        epoch: ServerEpoch::new(7),
        scope: Some(RepositoryScope {
            server,
            repository: RepositoryId::new(server, 1).unwrap(),
        }),
        result: GitQueryResult::Status(GitStatusFacts {
            revision: NonZeroU64::new(revision).unwrap(),
            branch: Some(spaceterm_core::organization::BranchName::new("main".into()).unwrap()),
            changes: vec![],
            dirty,
            unchecked: false,
            pull_request: None,
            upstream: None,
            operation: None,
            counts: None,
        }),
    }
}
fn connect(
    store: &gpui::Entity<ServerGitStore>,
    peer: &Arc<Peer>,
    server: ServerId,
    cx: &mut TestAppContext,
) {
    store.update(cx, |store, cx| {
        store
            .connect(
                GitClient::new(server, peer.clone()).unwrap(),
                ServerEpoch::new(7),
                cx,
            )
            .unwrap()
    });
}
#[gpui::test]
fn equal_looking_repositories_remain_server_qualified(cx: &mut TestAppContext) {
    let a = ServerId::new([1; 16]);
    let b = ServerId::new([2; 16]);
    let (pa, sa) = peer(a);
    let (pb, sb) = peer(b);
    let store = cx.new(ServerGitStore::new);
    connect(&store, &pa, a, cx);
    connect(&store, &pb, b, cx);
    let ia = store.update(cx, |store, cx| {
        store
            .register_status(
                WorkspaceId::new(a, 1).unwrap(),
                GitStatusTarget::Pane(PaneId::new(a, 1).unwrap()),
                cx,
            )
            .unwrap()
    });
    let ib = store.update(cx, |store, cx| {
        store
            .register_status(
                WorkspaceId::new(b, 1).unwrap(),
                GitStatusTarget::Pane(PaneId::new(b, 1).unwrap()),
                cx,
            )
            .unwrap()
    });
    sa.try_send(Ok(GitSubscriptionEvent::Status(Box::new(status(
        a, 1, true,
    )))))
    .unwrap();
    sb.try_send(Ok(GitSubscriptionEvent::Status(Box::new(status(
        b, 1, false,
    )))))
    .unwrap();
    cx.run_until_parked();
    store.read_with(cx, |store, _| {
        let va = store.status(ia).unwrap();
        let vb = store.status(ib).unwrap();
        assert!(va.facts.as_ref().is_some_and(|facts| facts.dirty));
        assert!(vb.facts.as_ref().is_some_and(|facts| !facts.dirty));
        assert_eq!(va.scope.unwrap().server, a);
        assert_eq!(vb.scope.unwrap().server, b);
        assert_eq!(va.freshness, GitFreshness::Current);
    });
}
#[gpui::test]
fn foreign_server_interest_is_rejected_before_subscribing(cx: &mut TestAppContext) {
    let a = ServerId::new([1; 16]);
    let b = ServerId::new([2; 16]);
    let store = cx.new(ServerGitStore::new);
    assert_eq!(
        store.update(cx, |store, cx| store.register_status(
            WorkspaceId::new(a, 1).unwrap(),
            GitStatusTarget::Pane(PaneId::new(b, 1).unwrap()),
            cx
        )),
        Err(ClientGitFailure::WrongServer)
    );
}

fn inventory(server: ServerId, revision: u64, worktree: u64) -> GitReadResult {
    GitReadResult {
        epoch: ServerEpoch::new(7),
        scope: Some(RepositoryScope {
            server,
            repository: RepositoryId::new(server, 1).unwrap(),
        }),
        result: GitQueryResult::Inventory(GitInventoryPage {
            version: OrganizationRevision::new(revision),
            next: None,
            records: vec![GitInventoryRecord {
                worktree: WorktreeId::new(server, worktree).unwrap(),
                directory: ServerDirectory::new(server, b"/same/display/path".to_vec()).unwrap(),
                main: false,
                missing: false,
                listed: true,
                version: FieldVersion::new(1),
            }],
        }),
    }
}

#[gpui::test]
fn gaps_resync_status_and_old_publications_cannot_replace_it(cx: &mut TestAppContext) {
    let server = ServerId::new([1; 16]);
    let (peer, send) = peer(server);
    let store = cx.new(ServerGitStore::new);
    connect(&store, &peer, server, cx);
    let id = store.update(cx, |store, cx| {
        store
            .register_status(
                WorkspaceId::new(server, 1).unwrap(),
                GitStatusTarget::Pane(PaneId::new(server, 1).unwrap()),
                cx,
            )
            .unwrap()
    });
    send.try_send(Ok(GitSubscriptionEvent::Status(Box::new(status(
        server, 1, true,
    )))))
    .unwrap();
    cx.run_until_parked();
    peer.reads
        .lock()
        .unwrap()
        .push_back(status(server, 2, false));
    send.try_send(Ok(GitSubscriptionEvent::ResyncRequired))
        .unwrap();
    cx.run_until_parked();
    send.try_send(Ok(GitSubscriptionEvent::Status(Box::new(status(
        server, 1, true,
    )))))
    .unwrap();
    cx.run_until_parked();
    store.read_with(cx, |store, _| {
        let view = store.status(id).unwrap();
        assert_eq!(view.facts.unwrap().revision.get(), 2);
        assert_eq!(view.freshness, GitFreshness::Current);
    });
    assert_eq!(peer.queries.load(Ordering::SeqCst), 1);
    send.try_send(Ok(GitSubscriptionEvent::LastKnown(GitFailure::ToolMissing)))
        .unwrap();
    cx.run_until_parked();
    store.read_with(cx, |store, _| {
        let view = store.status(id).unwrap();
        assert_eq!(view.facts.unwrap().revision.get(), 2);
        assert_eq!(view.freshness, GitFreshness::LastKnown);
    });
    store.update(cx, |store, cx| store.connection_lost(server, cx));
    cx.run_until_parked();
    assert_eq!(peer.drops.load(Ordering::SeqCst), 1);
    assert!(matches!(
        store.read_with(cx, |store, _| store.requests(server)),
        Err(ClientGitFailure::Offline)
    ));
    connect(&store, &peer, server, cx);
    send.try_send(Ok(GitSubscriptionEvent::Status(Box::new(status(
        server, 2, false,
    )))))
    .unwrap();
    cx.run_until_parked();
    store.read_with(cx, |store, _| {
        assert_eq!(store.status(id).unwrap().freshness, GitFreshness::Current)
    });
}

#[gpui::test]
fn inactive_inventory_updates_and_gaps_keep_last_complete_revision(cx: &mut TestAppContext) {
    let server = ServerId::new([1; 16]);
    let (peer, _send) = peer(server);
    let store = cx.new(ServerGitStore::new);
    connect(&store, &peer, server, cx);
    peer.reads
        .lock()
        .unwrap()
        .push_back(inventory(server, 2, 1));
    let id = store.update(cx, |store, cx| {
        store
            .register_inventory(WorkspaceId::new(server, 1).unwrap(), cx)
            .unwrap()
    });
    cx.run_until_parked();
    store.read_with(cx, |store, _| {
        let view = store.inventory(id).unwrap();
        assert_eq!(view.version, Some(OrganizationRevision::new(2)));
        assert_eq!(view.scope.unwrap().server, server);
    });
    peer.reads
        .lock()
        .unwrap()
        .push_back(inventory(server, 4, 2));
    store.update(cx, |store, cx| {
        store.organization_changed(
            server,
            ServerEpoch::new(7),
            OrganizationRevision::new(3),
            OrganizationRevision::new(5),
            cx,
        )
    });
    cx.run_until_parked();
    store.read_with(cx, |store, _| {
        let view = store.inventory(id).unwrap();
        assert_eq!(view.records[0].worktree.local(), 1);
        assert_eq!(view.version, Some(OrganizationRevision::new(2)));
        assert_eq!(view.freshness, GitFreshness::ResyncRequired);
    });
    peer.reads
        .lock()
        .unwrap()
        .push_back(inventory(server, 5, 2));
    store.update(cx, |store, cx| store.refresh(id, cx));
    cx.run_until_parked();
    store.read_with(cx, |store, _| {
        let view = store.inventory(id).unwrap();
        assert_eq!(view.records[0].worktree.local(), 2);
        assert_eq!(view.version, Some(OrganizationRevision::new(5)));
        assert_eq!(view.freshness, GitFreshness::Current);
    });
    store.update(cx, |store, cx| store.connection_lost(server, cx));
    store.read_with(cx, |store, _| {
        let view = store.inventory(id).unwrap();
        assert_eq!(view.records[0].worktree.local(), 2);
        assert_eq!(view.freshness, GitFreshness::LastKnown);
    });
}

#[gpui::test]
fn detach_and_deletion_drop_receivers_and_cancel_pending_reads(cx: &mut TestAppContext) {
    let server = ServerId::new([1; 16]);
    let (peer, _send) = peer(server);
    let store = cx.new(ServerGitStore::new);
    connect(&store, &peer, server, cx);
    let workspace = WorkspaceId::new(server, 1).unwrap();
    let target = GitStatusTarget::Pane(PaneId::new(server, 1).unwrap());
    let status_id = store.update(cx, |store, cx| {
        store.register_status(workspace, target, cx).unwrap()
    });
    let (reply, pending) = async_channel::bounded(1);
    *peer.delayed.lock().unwrap() = Some(pending);
    let inventory_id = store.update(cx, |store, cx| {
        store.register_inventory(workspace, cx).unwrap()
    });
    cx.run_until_parked();
    assert!(!reply.is_closed());
    store.update(cx, |store, cx| store.target_deleted(target, cx));
    cx.run_until_parked();
    assert_eq!(peer.drops.load(Ordering::SeqCst), 1);
    store.read_with(cx, |store, _| assert!(store.status(status_id).is_none()));
    store.update(cx, |store, cx| store.detach(workspace, cx));
    cx.run_until_parked();
    assert!(reply.is_closed());
    store.read_with(cx, |store, _| {
        assert!(store.inventory(inventory_id).is_none())
    });
}

#[gpui::test]
fn local_pull_request_results_are_revision_bound_and_shared_hooks_use_one_store(
    cx: &mut TestAppContext,
) {
    let server = ServerId::new([1; 16]);
    let (peer, send) = peer(server);
    let store = cx.new(ServerGitStore::new);
    connect(&store, &peer, server, cx);
    cx.update(|cx| cx.set_global(InstalledServerGit(store.clone())));
    cx.update(|cx| {
        assert_eq!(
            super::super::repository_status_store::InstalledRepositoryStatus::server_store(cx)
                .unwrap(),
            store
        );
        assert_eq!(
            super::super::worktree_store::InstalledWorktrees::server_store(cx).unwrap(),
            store
        );
    });
    let notices = Arc::new(Mutex::new(Vec::new()));
    let changed = notices.clone();
    let _subscription = cx.update(|cx| {
        cx.subscribe(&store, move |_, event: &ServerGitChanged, _| {
            changed.lock().unwrap().push(event.0)
        })
    });
    let id = store.update(cx, |store, cx| {
        store
            .register_status(
                WorkspaceId::new(server, 1).unwrap(),
                GitStatusTarget::Workspace(WorkspaceId::new(server, 1).unwrap()),
                cx,
            )
            .unwrap()
    });
    let mut read = status(server, 1, false);
    if let GitQueryResult::Status(facts) = &mut read.result {
        facts.pull_request = Some(
            GitPullRequestFacts::new(
                "github.com".into(),
                "person".into(),
                "repository".into(),
                facts.branch.clone().unwrap(),
                None,
            )
            .unwrap(),
        );
    }
    send.try_send(Ok(GitSubscriptionEvent::Status(Box::new(read))))
        .unwrap();
    cx.run_until_parked();
    let ticket = store.read_with(cx, |store, _| store.pull_request_ticket(id).unwrap());
    assert!(store.update(cx, |store, cx| store.apply_pull_request(
        &ticket,
        Err(PullRequestError::NotLoggedIn),
        cx
    )));
    store.read_with(cx, |store, _| {
        assert_eq!(
            store.status(id).unwrap().pull_request,
            Some(Err(PullRequestError::NotLoggedIn))
        )
    });
    send.try_send(Ok(GitSubscriptionEvent::Status(Box::new(status(
        server, 2, false,
    )))))
    .unwrap();
    cx.run_until_parked();
    assert!(!store.update(cx, |store, cx| store.apply_pull_request(
        &ticket,
        Ok(None),
        cx
    )));
    assert_eq!(peer.submissions.load(Ordering::SeqCst), 0);
    assert!(notices.lock().unwrap().contains(&id));
    store.update(cx, |store, cx| store.unregister(id, cx));
}

fn assessment(server: ServerId) -> GitAssessmentToken {
    GitAssessmentToken::new(GitAssessmentTokenParts {
        server,
        epoch: ServerEpoch::new(7),
        pairing: PairingId::new(server, 3).unwrap(),
        client: ClientId::new(server, 4).unwrap(),
        kind: GitAssessmentKind::Remove,
        random: [8; 32],
    })
    .unwrap()
}

#[gpui::test]
fn offline_writes_allocate_nothing_and_unresolved_writes_resume_only_original_id(
    cx: &mut TestAppContext,
) {
    let server = ServerId::new([1; 16]);
    let (peer, _send) = peer(server);
    let store = cx.new(ServerGitStore::new);
    connect(&store, &peer, server, cx);
    let requests = store.read_with(cx, |store, _| store.requests(server).unwrap());
    assert_eq!(requests.server(), server);
    let command = GitCommand::RemoveConfirmed {
        assessment: assessment(server),
    };
    peer.state.lock().unwrap().readiness = GitReadiness::Offline;
    assert!(matches!(
        pollster::block_on(requests.submit(command.clone())),
        Err(ClientGitFailure::Offline)
    ));
    assert_eq!(peer.submissions.load(Ordering::SeqCst), 0);
    peer.state.lock().unwrap().readiness = GitReadiness::Ready;
    let id = OperationId::new(
        OperationStreamId::new(
            OperationStreamKey::new(server, 1).unwrap(),
            PairingId::new(server, 3).unwrap(),
            ClientId::new(server, 4).unwrap(),
        )
        .unwrap(),
        spaceterm_core::operations::OperationSequence::new(1).unwrap(),
    );
    let original = Arc::new(Operation {
        id,
        command: command.clone(),
        state: Mutex::new(GitOperationState::Unresolved),
    });
    *peer.operation.lock().unwrap() = Some(original.clone());
    let operation = pollster::block_on(requests.submit(command)).unwrap();
    assert_eq!(operation.id(), id);
    assert_eq!(
        pollster::block_on(operation.state()).unwrap(),
        GitOperationState::Unresolved
    );
    drop(operation);
    store.update(cx, |store, cx| {
        store.detach(WorkspaceId::new(server, 1).unwrap(), cx)
    });
    *original.state.lock().unwrap() = GitOperationState::Completed(
        GitMutationOutcome::new(GitMutationOutcomeParts::Removed {
            worktree: WorktreeId::new(server, 1).unwrap(),
            closed_tabs: 1,
            ended_sessions: 1,
            revision: OrganizationRevision::new(9),
        })
        .unwrap(),
    );
    let recovered = pollster::block_on(requests.resume(id)).unwrap();
    assert_eq!(recovered.id(), id);
    assert!(matches!(
        pollster::block_on(recovered.state()).unwrap(),
        GitOperationState::Completed(_)
    ));
    assert_eq!(peer.submissions.load(Ordering::SeqCst), 1);
    assert_eq!(*peer.lookups.lock().unwrap(), [id]);
}

#[gpui::test]
fn forms_capture_the_server_and_never_use_native_paths(cx: &mut TestAppContext) {
    let server = ServerId::new([1; 16]);
    let (peer, _send) = peer(server);
    let store = cx.new(ServerGitStore::new);
    connect(&store, &peer, server, cx);
    let requests = store.read_with(cx, |store, _| store.requests(server).unwrap());
    let repository = RepositoryScope {
        server,
        repository: RepositoryId::new(server, 1).unwrap(),
    };
    assert_eq!(
        pollster::block_on(requests.branches(repository, None)),
        Err(ClientGitFailure::Unavailable)
    );
    let draft = WorktreeCreateDraft::new(
        repository,
        WorktreeBranchChoice::Existing {
            name: spaceterm_core::organization::BranchName::new("main".into()).unwrap(),
        },
        WorktreeLocationChoice::ServerTemplate {
            expected_version: FieldVersion::new(3),
        },
    )
    .unwrap();
    assert_eq!(
        pollster::block_on(requests.preview(draft.clone())),
        Err(ClientGitFailure::Unavailable)
    );
    assert_eq!(
        pollster::block_on(requests.assess_remove(WorktreeId::new(server, 1).unwrap())),
        Err(ClientGitFailure::Unavailable)
    );
    assert_eq!(
        pollster::block_on(requests.assessment_page(assessment(server), None)),
        Err(ClientGitFailure::Unavailable)
    );
    assert_eq!(
        peer.requests.lock().unwrap().as_slice(),
        [
            GitQuery::Branches {
                repository,
                cursor: None
            },
            GitQuery::PreviewCreate { request: draft },
            GitQuery::AssessRemove {
                worktree: WorktreeId::new(server, 1).unwrap()
            },
            GitQuery::AssessmentPage {
                assessment: assessment(server),
                cursor: None
            }
        ]
    );
    assert_eq!(peer.submissions.load(Ordering::SeqCst), 0);
}

#[gpui::test]
fn server_status_runs_only_client_github_lookup_and_reuses_it_across_status_revisions(
    cx: &mut TestAppContext,
) {
    use crate::repository_status::testing::{FakeRunner, exit};
    use std::path::{Path, PathBuf};

    let server = ServerId::new([1; 16]);
    let (peer, send) = peer(server);
    const LISTED: &str = r#"[{"number":520,"title":"Persistent servers","isDraft":false,
        "url":"https://github.com/person/repository/pull/520","headRefName":"main",
        "baseRefName":"main","headRepositoryOwner":{"login":"person"},"isCrossRepository":false}]"#;
    let runner = FakeRunner::new([exit(0, LISTED), exit(0, LISTED)]);
    let lookup = Arc::new(GitHubCliLookup::new(
        runner.clone(),
        PathBuf::from("/client-tools/gh"),
        PathBuf::from("/client-home"),
        Vec::new(),
    ));
    let store = cx.new(ServerGitStore::new);
    store.update(cx, |store, cx| {
        store
            .configure_pull_requests(
                RepositoryStatusPreferences::default(),
                GitHubCliStatus::SignedIn,
                ServerPullRequestAdapters {
                    lookup: Some(lookup),
                    hosts: Arc::default(),
                    spawn: Arc::new(|work| {
                        work();
                        true
                    }),
                },
                cx,
            )
            .unwrap();
    });
    connect(&store, &peer, server, cx);
    let id = store.update(cx, |store, cx| {
        store
            .register_status(
                WorkspaceId::new(server, 1).unwrap(),
                GitStatusTarget::Pane(PaneId::new(server, 1).unwrap()),
                cx,
            )
            .unwrap()
    });
    let mut read = status(server, 1, false);
    if let GitQueryResult::Status(facts) = &mut read.result {
        facts.pull_request = Some(
            GitPullRequestFacts::new(
                "github.com".into(),
                "person".into(),
                "repository".into(),
                BranchName::new("main".into()).unwrap(),
                None,
            )
            .unwrap(),
        );
    }
    send.try_send(Ok(GitSubscriptionEvent::Status(Box::new(read.clone()))))
        .unwrap();
    cx.run_until_parked();
    let calls = runner.requests();
    assert_eq!(
        calls.len(),
        1,
        "server facts must trigger one client-local lookup"
    );
    assert_eq!(calls[0].executable, Path::new("/client-tools/gh"));
    assert_eq!(calls[0].directory, Path::new("/client-home"));
    store.read_with(cx, |store, _| {
        assert_eq!(
            store
                .status(id)
                .unwrap()
                .pull_request
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()
                .as_ref()
                .unwrap()
                .number,
            520
        );
    });
    if let GitQueryResult::Status(facts) = &mut read.result {
        facts.revision = NonZeroU64::new(2).unwrap();
        facts.dirty = true;
    }
    send.try_send(Ok(GitSubscriptionEvent::Status(Box::new(read.clone()))))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(
        runner.requests().len(),
        1,
        "a status count change must not rerun gh"
    );
    store.read_with(cx, |store, _| {
        assert!(store.status(id).unwrap().pull_request.is_some());
    });
    store.update(cx, |store, cx| store.window_activated([id], cx).unwrap());
    cx.run_until_parked();
    assert_eq!(
        runner.requests().len(),
        2,
        "window activation refreshes the visible target"
    );
    read.scope = None;
    if let GitQueryResult::Status(facts) = &mut read.result {
        facts.revision = NonZeroU64::new(3).unwrap();
        facts.branch = None;
        facts.pull_request = None;
        facts.dirty = false;
    }
    send.try_send(Ok(GitSubscriptionEvent::Status(Box::new(read))))
        .unwrap();
    cx.run_until_parked();
    store.read_with(cx, |store, _| {
        let view = store.status(id).unwrap();
        assert!(view.scope.is_none());
        assert!(
            view.pull_request.is_none(),
            "leaving a repository must hide its old Pull Request"
        );
    });
    assert_eq!(peer.submissions.load(Ordering::SeqCst), 0);
}

#[gpui::test]
fn detach_cancels_client_local_pr_lookup_without_starting_native_git(cx: &mut TestAppContext) {
    use crate::repository_status::testing::FakeRunner;
    use std::path::PathBuf;

    let server = ServerId::new([1; 16]);
    let (peer, send) = peer(server);
    let runner = FakeRunner::new([]); // Any native program call is a test failure.
    let pending = Arc::new(Mutex::new(VecDeque::<Box<dyn FnOnce() + Send>>::new()));
    let queued = pending.clone();
    let store = cx.new(ServerGitStore::new);
    store.update(cx, |store, cx| {
        store
            .configure_pull_requests(
                RepositoryStatusPreferences::default(),
                GitHubCliStatus::SignedIn,
                ServerPullRequestAdapters {
                    lookup: Some(Arc::new(GitHubCliLookup::new(
                        runner.clone(),
                        PathBuf::from("/client-tools/gh"),
                        PathBuf::from("/client-home"),
                        Vec::new(),
                    ))),
                    hosts: Arc::default(),
                    spawn: Arc::new(move |work| {
                        queued.lock().unwrap().push_back(work);
                        true
                    }),
                },
                cx,
            )
            .unwrap();
    });
    connect(&store, &peer, server, cx);
    let workspace = WorkspaceId::new(server, 1).unwrap();
    let id = store.update(cx, |store, cx| {
        store
            .register_status(
                workspace,
                GitStatusTarget::Pane(PaneId::new(server, 1).unwrap()),
                cx,
            )
            .unwrap()
    });
    let mut read = status(server, 1, false);
    if let GitQueryResult::Status(facts) = &mut read.result {
        facts.pull_request = Some(
            GitPullRequestFacts::new(
                "github.com".into(),
                "person".into(),
                "repository".into(),
                BranchName::new("main".into()).unwrap(),
                None,
            )
            .unwrap(),
        );
    }
    send.try_send(Ok(GitSubscriptionEvent::Status(Box::new(read))))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(pending.lock().unwrap().len(), 1);
    store.update(cx, |store, cx| store.detach(workspace, cx));
    pending.lock().unwrap().pop_front().unwrap()();
    cx.run_until_parked();
    assert!(runner.requests().is_empty());
    assert_eq!(peer.drops.load(Ordering::SeqCst), 1);
    store.read_with(cx, |store, _| assert!(store.status(id).is_none()));
}

#[gpui::test]
fn installing_the_shared_server_store_preserves_existing_interests(cx: &mut TestAppContext) {
    let store = cx.update(InstalledServerGit::install);
    let server = ServerId::new([1; 16]);
    let id = store.update(cx, |store, cx| {
        store
            .register_inventory(WorkspaceId::new(server, 1).unwrap(), cx)
            .unwrap()
    });
    let second = cx.update(InstalledServerGit::install);
    assert_eq!(second, store);
    second.read_with(cx, |store, _| {
        assert_eq!(
            store.inventory(id).unwrap().freshness,
            GitFreshness::LastKnown
        );
    });
}

#[gpui::test]
fn incomplete_inventory_keeps_last_complete_projection_and_retries_all_pages(
    cx: &mut TestAppContext,
) {
    let server = ServerId::new([1; 16]);
    let (peer, _send) = peer(server);
    let store = cx.new(ServerGitStore::new);
    connect(&store, &peer, server, cx);
    let workspace = WorkspaceId::new(server, 1).unwrap();
    peer.reads
        .lock()
        .unwrap()
        .push_back(inventory(server, 1, 1));
    let id = store.update(cx, |store, cx| {
        store.register_inventory(workspace, cx).unwrap()
    });
    cx.run_until_parked();
    let cursor = GitPageCursor::new(GitPageCursorParts {
        scope: RepositoryScope {
            server,
            repository: RepositoryId::new(server, 1).unwrap(),
        },
        epoch: ServerEpoch::new(7),
        observation: 2,
        random: [9; 32],
    })
    .unwrap();
    let mut first = inventory(server, 2, 2);
    if let GitQueryResult::Inventory(page) = &mut first.result {
        page.next = Some(cursor.clone());
    }
    peer.reads.lock().unwrap().push_back(first.clone());
    store.update(cx, |store, cx| store.refresh(id, cx));
    cx.run_until_parked();
    store.read_with(cx, |store, _| {
        let view = store.inventory(id).unwrap();
        assert_eq!(view.version, Some(OrganizationRevision::new(1)));
        assert_eq!(view.records[0].worktree.local(), 1);
        assert_eq!(view.freshness, GitFreshness::ResyncRequired);
    });
    assert_eq!(
        peer.requests.lock().unwrap().last(),
        Some(&GitQuery::Inventory {
            workspace,
            cursor: Some(cursor),
        })
    );
    peer.reads
        .lock()
        .unwrap()
        .extend([first, inventory(server, 2, 3)]);
    store.update(cx, |store, cx| store.refresh(id, cx));
    cx.run_until_parked();
    store.read_with(cx, |store, _| {
        let view = store.inventory(id).unwrap();
        assert_eq!(view.version, Some(OrganizationRevision::new(2)));
        assert_eq!(
            view.records
                .iter()
                .map(|record| record.worktree.local())
                .collect::<Vec<_>>(),
            [2, 3]
        );
        assert_eq!(view.freshness, GitFreshness::Current);
    });
}

#[gpui::test]
fn organization_changes_cancel_obsolete_inventory_reads(cx: &mut TestAppContext) {
    let server = ServerId::new([1; 16]);
    let (peer, _send) = peer(server);
    let store = cx.new(ServerGitStore::new);
    connect(&store, &peer, server, cx);
    peer.reads
        .lock()
        .unwrap()
        .push_back(inventory(server, 1, 1));
    let id = store.update(cx, |store, cx| {
        store
            .register_inventory(WorkspaceId::new(server, 1).unwrap(), cx)
            .unwrap()
    });
    cx.run_until_parked();
    let (late_reply, pending) = async_channel::bounded(1);
    *peer.delayed.lock().unwrap() = Some(pending);
    store.update(cx, |store, cx| store.refresh(id, cx));
    cx.run_until_parked();
    assert!(!late_reply.is_closed());
    peer.reads
        .lock()
        .unwrap()
        .push_back(inventory(server, 3, 3));
    store.update(cx, |store, cx| {
        store.organization_changed(
            server,
            ServerEpoch::new(7),
            OrganizationRevision::new(1),
            OrganizationRevision::new(3),
            cx,
        )
    });
    cx.run_until_parked();
    assert!(late_reply.is_closed());
    store.read_with(cx, |store, _| {
        let view = store.inventory(id).unwrap();
        assert_eq!(view.version, Some(OrganizationRevision::new(3)));
        assert_eq!(view.records[0].worktree.local(), 3);
        assert_eq!(view.freshness, GitFreshness::Current);
    });
}

#[gpui::test]
fn canonical_targets_share_receivers_until_the_last_local_subscriber_leaves(
    cx: &mut TestAppContext,
) {
    let server = ServerId::new([1; 16]);
    let (peer, _send) = peer(server);
    let store = cx.new(ServerGitStore::new);
    connect(&store, &peer, server, cx);
    let workspace = WorkspaceId::new(server, 1).unwrap();
    let target = GitStatusTarget::Pane(PaneId::new(server, 1).unwrap());
    let first = store.update(cx, |store, cx| {
        store.register_status(workspace, target, cx).unwrap()
    });
    let second = store.update(cx, |store, cx| {
        store.register_status(workspace, target, cx).unwrap()
    });
    assert_eq!(first, second);
    cx.run_until_parked();
    store.update(cx, |store, cx| store.unregister(first, cx));
    cx.run_until_parked();
    assert_eq!(peer.drops.load(Ordering::SeqCst), 0);
    store.read_with(cx, |store, _| assert!(store.status(second).is_some()));
    store.update(cx, |store, cx| store.unregister(second, cx));
    cx.run_until_parked();
    assert_eq!(peer.drops.load(Ordering::SeqCst), 1);
    store.read_with(cx, |store, _| assert!(store.status(second).is_none()));

    peer.reads
        .lock()
        .unwrap()
        .push_back(inventory(server, 1, 1));
    let first = store.update(cx, |store, cx| {
        store.register_inventory(workspace, cx).unwrap()
    });
    let second = store.update(cx, |store, cx| {
        store.register_inventory(workspace, cx).unwrap()
    });
    assert_eq!(first, second);
    cx.run_until_parked();
    assert_eq!(peer.queries.load(Ordering::SeqCst), 1);
    store.update(cx, |store, cx| store.detach(workspace, cx));
    store.read_with(cx, |store, _| assert!(store.inventory(second).is_none()));
}
