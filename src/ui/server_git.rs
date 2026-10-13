//! Application-scoped Git read projections over the owning server's client port.
//! Shared membership and original write identities stay in client-core. This
//! module accepts no native Git, SSH utility, or client filesystem adapter.

#![cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "desktop consumer composition installs these hooks"
    )
)]

use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::Instant;

use gpui::{App, AppContext as _, Context, Entity, EventEmitter, Global, Task};
use spaceterm_client::git::{
    replica::{GitFreshness, GitInventoryReplica, GitStatusReplica, InventoryProgress},
    rpc::{ClientGitFailure, GitClient, GitReadResult, GitSubscriptionEvent, TrackedGitOperation},
};
use spaceterm_core::{
    authority::{OrganizationRevision, ServerEpoch},
    identity::{ServerId, WorkspaceId, WorktreeId},
    operations::{OperationId, RepositoryScope},
    organization::BranchName,
    worktrees::{
        GitAssessmentToken, GitCommand, GitPageCursor, GitQuery, GitStatusTarget,
        MAX_GIT_REPOSITORIES, WorktreeCreateDraft,
        queries::{
            GitAssessmentPage, GitBranchPage, GitInventoryRecord, GitPullRequestFacts,
            GitQueryResult, GitStatusFacts,
        },
    },
};

use super::repository_status_store::{ReadSpawner, thread_spawner};
use crate::repository_status::pull_request_lookup::{GitHubCliLookup, GitHubHosts};
use crate::repository_status::server_pull_request::{
    ServerPullRequestTicket as ClientPullRequestTicket, ServerPullRequests,
};
use crate::repository_status::{
    GitHubCliStatus, PullRequest, PullRequestError, RepositoryStatusPreferences,
};

/// Client-local GitHub capability only. No repository discovery, marker reader,
/// native Git executable or server directory is accepted by this composition.
pub(crate) struct ServerPullRequestAdapters {
    pub(crate) lookup: Option<Arc<GitHubCliLookup>>,
    pub(crate) hosts: Arc<GitHubHosts>,
    pub(crate) spawn: ReadSpawner,
}

/// A local receiver lifetime, never a protocol entity or operation identity.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct ServerGitInterest(u64);

#[derive(Clone, Debug)]
pub(crate) struct ServerStatusView {
    pub(crate) scope: Option<RepositoryScope>,
    pub(crate) facts: Option<GitStatusFacts>,
    pub(crate) freshness: GitFreshness,
    pub(crate) read_at: Instant,
    /// Client-local presentation only. It is never submitted to organization.
    pub(crate) pull_request: Option<Result<Option<PullRequest>, PullRequestError>>,
}

impl ServerStatusView {
    /// Presentation only. Even private IPC directories remain opaque remote roots.
    pub(crate) fn repository_view(
        &self,
        directory: &spaceterm_core::organization::ServerDirectory,
    ) -> crate::repository_status::RepositoryView {
        use crate::repository_status::{
            ChangeState, ChangeSummary, ChangeTotal, Freshness, RepositoryHead, RepositoryKey,
            RepositoryMachine, RepositoryRoot, RepositoryStatus, RepositoryView, Upstream,
        };
        use spaceterm_core::worktrees::queries::{GitChangeTotal, GitRepositoryOperation};
        use spaceterm_host::git::{
            ChangeEntry, ChangeKind, Divergence, OperationStep, RepositoryOperation,
        };
        let (Some(scope), Some(facts)) = (self.scope, self.facts.as_ref()) else {
            return RepositoryView::Hidden;
        };
        if scope.server != directory.server() {
            return RepositoryView::Hidden;
        }
        let now = self.read_at;
        let step = |step: spaceterm_core::worktrees::queries::GitOperationStep| OperationStep {
            current: step.current,
            total: step.total,
        };
        let operation = facts.operation.map(|operation| match operation {
            GitRepositoryOperation::Rebasing { step: progress } => RepositoryOperation::Rebasing {
                step: progress.map(step),
            },
            GitRepositoryOperation::Applying { step: progress } => RepositoryOperation::Applying {
                step: progress.map(step),
            },
            GitRepositoryOperation::Merging => RepositoryOperation::Merging,
            GitRepositoryOperation::Reverting => RepositoryOperation::Reverting,
            GitRepositoryOperation::CherryPicking => RepositoryOperation::CherryPicking,
            GitRepositoryOperation::Bisecting => RepositoryOperation::Bisecting,
        });
        let changes = facts.counts.map_or(ChangeState::NotCounted, |counts| {
            ChangeState::Known(ChangeSummary {
                total: match counts.total {
                    GitChangeTotal::Exact(n) => ChangeTotal::Exact(n),
                    GitChangeTotal::AtLeast(n) => ChangeTotal::AtLeast(n),
                },
                staged: counts.staged,
                modified: counts.modified,
                deleted: counts.deleted,
                renamed: counts.renamed,
                conflicted: counts.conflicted,
                untracked: counts.untracked,
                entries: facts
                    .changes
                    .iter()
                    .filter_map(|change| {
                        let kind = match change.kind {
                            1 => ChangeKind::Staged,
                            2 => ChangeKind::Modified,
                            3 => ChangeKind::Deleted,
                            4 => ChangeKind::Renamed,
                            5 => ChangeKind::Conflicted,
                            6 => ChangeKind::Untracked,
                            _ => return None,
                        };
                        Some(ChangeEntry {
                            kind,
                            path: crate::repository_status::presentation::sanitize_for_display(
                                &String::from_utf8_lossy(change.path()),
                                crate::repository_status::presentation::MAXIMUM_PATH_CHARS,
                            )
                            .into(),
                        })
                    })
                    .collect(),
            })
        });
        RepositoryView::Repository(Arc::new(RepositoryStatus {
            key: RepositoryKey {
                machine: RepositoryMachine::Remote(
                    crate::repository_status::RemoteMachineKey::new(format!(
                        "server-{:032x}",
                        u128::from_be_bytes(scope.server.bytes())
                    )),
                ),
                root: RepositoryRoot::Remote(
                    String::from_utf8_lossy(directory.bytes())
                        .into_owned()
                        .into(),
                ),
            },
            head: facts.branch.as_ref().map_or_else(
                || RepositoryHead::Detached("HEAD".into()),
                |branch| RepositoryHead::Branch(branch.as_str().into()),
            ),
            commit: None,
            upstream: facts.upstream.as_ref().map(|upstream| Upstream {
                name: upstream.name.as_str().into(),
                divergence: upstream.divergence.map(|value| Divergence {
                    ahead: value.ahead,
                    behind: value.behind,
                }),
            }),
            operation,
            changes,
            freshness: if self.freshness == GitFreshness::Current && !facts.unchecked {
                Freshness::Current
            } else {
                Freshness::LastKnown { as_of: now }
            },
            read_at: now,
            pull_request: self
                .pull_request
                .as_ref()
                .and_then(|result| result.as_ref().ok())
                .and_then(Clone::clone),
            read_failure: None,
        }))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ServerInventoryView {
    pub(crate) scope: Option<RepositoryScope>,
    pub(crate) records: Vec<GitInventoryRecord>,
    pub(crate) version: Option<OrganizationRevision>,
    pub(crate) freshness: GitFreshness,
}

/// Captures the canonical repository and exact facts used by a client-local lookup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ServerPullRequestTicket {
    pub(crate) interest: ServerGitInterest,
    pub(crate) repository: RepositoryScope,
    pub(crate) epoch: ServerEpoch,
    pub(crate) revision: NonZeroU64,
    pub(crate) branch: BranchName,
    pub(crate) facts: GitPullRequestFacts,
}

pub(crate) struct ServerGitChanged(pub(crate) ServerGitInterest);

#[derive(Clone)]
pub(crate) struct InstalledServerGit(pub(crate) Entity<ServerGitStore>);
impl Global for InstalledServerGit {}
impl InstalledServerGit {
    pub(crate) fn install(cx: &mut App) -> Entity<ServerGitStore> {
        if let Some(store) = Self::store(cx) {
            return store;
        }
        let store = cx.new(ServerGitStore::new);
        cx.set_global(Self(store.clone()));
        store
    }

    pub(crate) fn store(cx: &App) -> Option<Entity<ServerGitStore>> {
        cx.try_global::<Self>().map(|installed| installed.0.clone())
    }
}

struct Connection {
    client: Option<GitClient>,
    epoch: ServerEpoch,
}
struct StatusInterest {
    references: usize,
    workspace: WorkspaceId,
    target: GitStatusTarget,
    epoch: ServerEpoch,
    replica: GitStatusReplica,
    read_at: Instant,
    pull_request: Option<Result<Option<PullRequest>, PullRequestError>>,
    generation: u64,
    task: Option<Task<()>>,
}
struct InventoryInterest {
    references: usize,
    workspace: WorkspaceId,
    replica: GitInventoryReplica,
    offline: bool,
    generation: u64,
    task: Option<Task<()>>,
}

pub(crate) struct ServerGitStore {
    servers: BTreeMap<ServerId, Connection>,
    statuses: BTreeMap<ServerGitInterest, StatusInterest>,
    inventories: BTreeMap<ServerGitInterest, InventoryInterest>,
    next: u64,
    pull_requests: ServerPullRequests,
    pull_request_adapters: ServerPullRequestAdapters,
    presentation_enabled: bool,
}
impl EventEmitter<ServerGitChanged> for ServerGitStore {}
impl ServerGitStore {
    pub(crate) fn new(_: &mut Context<Self>) -> Self {
        Self {
            servers: BTreeMap::new(),
            statuses: BTreeMap::new(),
            inventories: BTreeMap::new(),
            next: 0,
            presentation_enabled: true,
            pull_requests: ServerPullRequests::new(
                RepositoryStatusPreferences::default(),
                GitHubCliStatus::Unknown,
            ),
            pull_request_adapters: ServerPullRequestAdapters {
                lookup: None,
                hosts: Arc::default(),
                spawn: thread_spawner(),
            },
        }
    }

    /// Installs client-local lookup policy without granting native Git authority.
    pub(crate) fn configure_pull_requests(
        &mut self,
        preferences: RepositoryStatusPreferences,
        github_cli: GitHubCliStatus,
        adapters: ServerPullRequestAdapters,
        cx: &mut Context<Self>,
    ) -> Result<(), ClientGitFailure> {
        self.presentation_enabled = preferences.show_repository_status;
        self.pull_request_adapters = adapters;
        let mut tickets = self
            .pull_requests
            .set_policy(preferences, github_cli, Instant::now())
            .map_err(|_| ClientGitFailure::Capacity)?;
        for interest in self.statuses.values() {
            if interest.replica.freshness() == GitFreshness::Current
                && let Some(scope) = interest.replica.scope()
                && let Some(facts) = interest.replica.facts()
            {
                tickets.extend(
                    self.pull_requests
                        .observe(interest.target, scope.repository, facts, Instant::now())
                        .map_err(|_| ClientGitFailure::Capacity)?,
                );
            }
        }
        self.run_pull_requests(tickets, cx);
        for id in self.statuses.keys() {
            cx.emit(ServerGitChanged(*id));
        }
        Ok(())
    }

    /// Window activation refreshes only the visible qualified targets. It never
    /// probes a client path or refreshes an unrelated server's repository.
    pub(crate) fn window_activated(
        &mut self,
        interests: impl IntoIterator<Item = ServerGitInterest>,
        cx: &mut Context<Self>,
    ) -> Result<(), ClientGitFailure> {
        let targets = interests
            .into_iter()
            .filter_map(|id| {
                self.statuses
                    .get(&id)
                    .filter(|interest| interest.replica.freshness() == GitFreshness::Current)
                    .map(|interest| interest.target)
            })
            .collect::<Vec<_>>();
        let tickets = self
            .pull_requests
            .window_activated(targets, Instant::now())
            .map_err(|_| ClientGitFailure::Capacity)?;
        self.run_pull_requests(tickets, cx);
        Ok(())
    }

    fn run_pull_requests(&mut self, tickets: Vec<ClientPullRequestTicket>, cx: &mut Context<Self>) {
        for ticket in tickets {
            let lookup = self.pull_request_adapters.lookup.clone();
            let hosts = self.pull_request_adapters.hosts.clone();
            let worker = ticket.clone();
            let (send, receive) = async_channel::bounded(1);
            let spawned = (self.pull_request_adapters.spawn)(Box::new(move || {
                let result = worker.run(lookup.as_deref(), &hosts);
                let _ = send.try_send(result);
            }));
            if !spawned {
                // A failed spawn still releases the scheduler's reserved slot.
                self.finish_pull_request(ticket, Err(PullRequestError::Unavailable), cx);
                continue;
            }
            cx.spawn(async move |store, cx| {
                if let Ok(result) = receive.recv().await {
                    let _ = store.update(cx, |store, cx| {
                        store.finish_pull_request(ticket, result, cx);
                    });
                }
            })
            .detach();
        }
    }

    fn finish_pull_request(
        &mut self,
        ticket: ClientPullRequestTicket,
        result: Result<Option<PullRequest>, PullRequestError>,
        cx: &mut Context<Self>,
    ) {
        let repository = ticket.repository();
        if let Ok(tickets) = self.pull_requests.finished(ticket, result, Instant::now()) {
            self.run_pull_requests(tickets, cx);
        }
        for (id, interest) in &self.statuses {
            if interest
                .replica
                .scope()
                .is_some_and(|scope| scope.repository == repository)
            {
                cx.emit(ServerGitChanged(*id));
            }
        }
    }

    fn forget_pull_request_target(&mut self, target: GitStatusTarget, cx: &mut Context<Self>) {
        if !self.statuses.values().any(|interest| {
            interest.target == target && interest.replica.freshness() == GitFreshness::Current
        }) && let Ok(tickets) = self.pull_requests.remove(target, Instant::now())
        {
            self.run_pull_requests(tickets, cx);
        }
    }

    /// Installs the application provider's existing per-server facade. Reconnect
    /// replaces receivers, not hierarchy or native identities.
    pub(crate) fn connect(
        &mut self,
        client: GitClient,
        epoch: ServerEpoch,
        cx: &mut Context<Self>,
    ) -> Result<(), ClientGitFailure> {
        if epoch.get() == 0 {
            return Err(ClientGitFailure::InvalidResponse);
        }
        let server = client.server();
        if self
            .servers
            .get(&server)
            .is_some_and(|old| old.epoch > epoch)
        {
            return Err(ClientGitFailure::StaleResponse);
        }
        if !self.servers.contains_key(&server) && self.servers.len() >= MAX_GIT_REPOSITORIES {
            return Err(ClientGitFailure::Capacity);
        }
        self.servers.insert(
            server,
            Connection {
                client: Some(client),
                epoch,
            },
        );
        for id in self.server_interests(server) {
            self.refresh(id, cx);
        }
        Ok(())
    }

    pub(crate) fn connection_lost(&mut self, server: ServerId, cx: &mut Context<Self>) {
        if let Some(connection) = self.servers.get_mut(&server) {
            connection.client = None;
        }
        for id in self.server_interests(server) {
            if let Some(interest) = self.statuses.get_mut(&id) {
                interest.task = None;
                interest.replica.mark_last_known();
                interest.pull_request = None;
            }
            if let Some(interest) = self.inventories.get_mut(&id) {
                interest.task = None;
                interest.replica.require_resync();
                interest.offline = true;
            }
            cx.emit(ServerGitChanged(id));
        }
        let targets = self
            .statuses
            .values()
            .filter(|interest| interest.workspace.server() == server)
            .map(|interest| interest.target)
            .collect::<Vec<_>>();
        for target in targets {
            self.forget_pull_request_target(target, cx);
        }
    }

    pub(crate) fn register_status(
        &mut self,
        workspace: WorkspaceId,
        target: GitStatusTarget,
        cx: &mut Context<Self>,
    ) -> Result<ServerGitInterest, ClientGitFailure> {
        if workspace.server() != target.server() {
            return Err(ClientGitFailure::WrongServer);
        }
        if let Some((id, interest)) = self
            .statuses
            .iter_mut()
            .find(|(_, interest)| interest.workspace == workspace && interest.target == target)
        {
            interest.references = interest
                .references
                .checked_add(1)
                .ok_or(ClientGitFailure::Capacity)?;
            return Ok(*id);
        }
        let id = self.allocate()?;
        let epoch = self
            .servers
            .get(&workspace.server())
            .map_or(ServerEpoch::new(0), |connection| connection.epoch);
        self.statuses.insert(
            id,
            StatusInterest {
                references: 1,
                workspace,
                target,
                epoch,
                replica: GitStatusReplica::new(target, epoch),
                read_at: Instant::now(),
                pull_request: None,
                generation: 0,
                task: None,
            },
        );
        self.refresh(id, cx);
        Ok(id)
    }

    /// Inventory subscriptions exist for inactive Workspaces too. No visibility
    /// flag can suppress organization invalidation or page application.
    pub(crate) fn register_inventory(
        &mut self,
        workspace: WorkspaceId,
        cx: &mut Context<Self>,
    ) -> Result<ServerGitInterest, ClientGitFailure> {
        if let Some((id, interest)) = self
            .inventories
            .iter_mut()
            .find(|(_, interest)| interest.workspace == workspace)
        {
            interest.references = interest
                .references
                .checked_add(1)
                .ok_or(ClientGitFailure::Capacity)?;
            return Ok(*id);
        }
        let id = self.allocate()?;
        self.inventories.insert(
            id,
            InventoryInterest {
                references: 1,
                workspace,
                replica: GitInventoryReplica::new(workspace),
                offline: false,
                generation: 0,
                task: None,
            },
        );
        self.refresh(id, cx);
        Ok(id)
    }

    /// Local lifetime check, independent of presentation policy and online admission.
    pub(crate) fn contains_interest(&self, id: ServerGitInterest) -> bool {
        self.statuses.contains_key(&id) || self.inventories.contains_key(&id)
    }

    pub(crate) fn status(&self, id: ServerGitInterest) -> Option<ServerStatusView> {
        if !self.presentation_enabled {
            return None;
        }
        self.statuses.get(&id).map(|interest| ServerStatusView {
            scope: interest.replica.scope(),
            facts: interest.replica.facts().cloned(),
            freshness: interest.replica.freshness(),
            read_at: interest.read_at,
            pull_request: interest.pull_request.clone().or_else(|| {
                (interest.replica.freshness() == GitFreshness::Current)
                    .then(|| self.pull_requests.view(interest.target).cloned())
                    .flatten()
                    .map(|value| Ok(Some(value)))
            }),
        })
    }
    pub(crate) fn inventory(&self, id: ServerGitInterest) -> Option<ServerInventoryView> {
        self.inventories
            .get(&id)
            .map(|interest| ServerInventoryView {
                scope: interest.replica.scope(),
                records: interest.replica.records().to_vec(),
                version: interest.replica.version(),
                freshness: if interest.offline {
                    GitFreshness::LastKnown
                } else if interest.replica.needs_resync() {
                    GitFreshness::ResyncRequired
                } else {
                    GitFreshness::Current
                },
            })
    }

    /// Cancels only reads. Dropping a form or detaching cannot cancel a Started
    /// write; its original operation remains in the existing native journal.
    pub(crate) fn unregister(&mut self, id: ServerGitInterest, cx: &mut Context<Self>) {
        let references = self
            .statuses
            .get_mut(&id)
            .map(|interest| &mut interest.references)
            .or_else(|| {
                self.inventories
                    .get_mut(&id)
                    .map(|interest| &mut interest.references)
            });
        if let Some(references) = references
            && *references > 1
        {
            *references -= 1;
            return;
        }
        self.remove_interest(id, cx);
    }

    fn remove_interest(&mut self, id: ServerGitInterest, cx: &mut Context<Self>) {
        if let Some(interest) = self.statuses.remove(&id) {
            self.forget_pull_request_target(interest.target, cx);
            cx.emit(ServerGitChanged(id));
        } else if self.inventories.remove(&id).is_some() {
            cx.emit(ServerGitChanged(id));
        }
    }
    pub(crate) fn detach(&mut self, workspace: WorkspaceId, cx: &mut Context<Self>) {
        let ids: Vec<_> = self
            .statuses
            .iter()
            .filter(|(_, interest)| interest.workspace == workspace)
            .map(|(id, _)| *id)
            .chain(
                self.inventories
                    .iter()
                    .filter(|(_, interest)| interest.workspace == workspace)
                    .map(|(id, _)| *id),
            )
            .collect();
        for id in ids {
            self.remove_interest(id, cx);
        }
    }
    pub(crate) fn target_deleted(&mut self, target: GitStatusTarget, cx: &mut Context<Self>) {
        let ids: Vec<_> = self
            .statuses
            .iter()
            .filter(|(_, interest)| interest.target == target)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.remove_interest(id, cx);
        }
    }

    /// The organization replica alone reduces membership. A gap or newer
    /// revision invalidates this read projection and requests a complete inventory.
    pub(crate) fn organization_changed(
        &mut self,
        server: ServerId,
        epoch: ServerEpoch,
        base: OrganizationRevision,
        revision: OrganizationRevision,
        cx: &mut Context<Self>,
    ) {
        if self
            .servers
            .get(&server)
            .is_none_or(|connection| connection.epoch != epoch)
        {
            return;
        }
        let ids: Vec<_> = self
            .inventories
            .iter_mut()
            .filter(|(_, interest)| interest.workspace.server() == server)
            .filter_map(|(id, interest)| {
                interest.replica.organization_changed(epoch, base, revision);
                interest.replica.needs_resync().then_some(*id)
            })
            .collect();
        for id in ids {
            self.refresh(id, cx);
        }
    }

    pub(crate) fn refresh(&mut self, id: ServerGitInterest, cx: &mut Context<Self>) {
        if self.statuses.contains_key(&id) {
            self.follow_status(id, cx);
        } else if self.inventories.contains_key(&id) {
            self.read_inventory(id, cx);
        }
    }

    pub(crate) fn pull_request_ticket(
        &self,
        id: ServerGitInterest,
    ) -> Option<ServerPullRequestTicket> {
        let interest = self.statuses.get(&id)?;
        if interest.replica.freshness() != GitFreshness::Current {
            return None;
        }
        let facts = interest.replica.facts()?;
        Some(ServerPullRequestTicket {
            interest: id,
            repository: interest.replica.scope()?,
            epoch: interest.epoch,
            revision: facts.revision,
            branch: facts.branch.clone()?,
            facts: facts.pull_request.clone()?,
        })
    }
    pub(crate) fn apply_pull_request(
        &mut self,
        ticket: &ServerPullRequestTicket,
        result: Result<Option<PullRequest>, PullRequestError>,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.pull_request_ticket(ticket.interest).as_ref() != Some(ticket) {
            return false;
        }
        if let Some(interest) = self.statuses.get_mut(&ticket.interest) {
            interest.pull_request = Some(result);
        }
        cx.emit(ServerGitChanged(ticket.interest));
        true
    }

    /// Captures the selected server now. Forms retain this facade through
    /// asynchronous preview, confirmation, submission and original-ID recovery.
    pub(crate) fn requests(&self, server: ServerId) -> Result<ServerGitRequests, ClientGitFailure> {
        let client = self
            .servers
            .get(&server)
            .and_then(|connection| connection.client.clone())
            .ok_or(ClientGitFailure::Offline)?;
        Ok(ServerGitRequests { client })
    }

    fn allocate(&mut self) -> Result<ServerGitInterest, ClientGitFailure> {
        if self.statuses.len() + self.inventories.len() >= MAX_GIT_REPOSITORIES {
            return Err(ClientGitFailure::Capacity);
        }
        self.next = self.next.checked_add(1).ok_or(ClientGitFailure::Capacity)?;
        Ok(ServerGitInterest(self.next))
    }
    fn server_interests(&self, server: ServerId) -> Vec<ServerGitInterest> {
        self.statuses
            .iter()
            .filter(|(_, interest)| interest.workspace.server() == server)
            .map(|(id, _)| *id)
            .chain(
                self.inventories
                    .iter()
                    .filter(|(_, interest)| interest.workspace.server() == server)
                    .map(|(id, _)| *id),
            )
            .collect()
    }

    fn follow_status(&mut self, id: ServerGitInterest, cx: &mut Context<Self>) {
        let Some(interest) = self.statuses.get_mut(&id) else {
            return;
        };
        interest.task = None;
        interest.pull_request = None;
        let Some(connection) = self
            .servers
            .get(&interest.workspace.server())
            .filter(|connection| connection.client.is_some())
        else {
            interest.replica.mark_last_known();
            cx.emit(ServerGitChanged(id));
            return;
        };
        let Some(generation) = interest.generation.checked_add(1) else {
            return;
        };
        interest.generation = generation;
        let epoch = connection.epoch;
        let client = connection.client.clone();
        let Some(client) = client else {
            return;
        };
        let target = interest.target;
        interest.replica.require_resync();
        let receiver = client.subscribe(target);
        let task = cx.spawn(async move |store, cx| {
            let Ok(mut receiver) = receiver else {
                let _ = store.update(cx, |store, cx| store.status_unavailable(id, generation, cx));
                return;
            };
            let mut first = true;
            while let Some(event) = receiver.next().await {
                match event {
                    Ok(GitSubscriptionEvent::Status(read)) => {
                        let snapshot = first;
                        first = false;
                        if store
                            .update(cx, |store, cx| {
                                store.accept_status(id, generation, epoch, *read, snapshot, cx)
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Ok(GitSubscriptionEvent::ResyncRequired) => {
                        let _ = store.update(cx, |store, cx| {
                            if let Some(interest) = store
                                .statuses
                                .get_mut(&id)
                                .filter(|interest| interest.generation == generation)
                            {
                                interest.replica.require_resync();
                                interest.pull_request = None;
                                cx.emit(ServerGitChanged(id));
                            }
                        });
                        let read = client.query(GitQuery::Status { target }).await;
                        if let Ok(read) = read {
                            let _ = store.update(cx, |store, cx| {
                                store.accept_status(id, generation, epoch, read, true, cx)
                            });
                        }
                    }
                    Ok(GitSubscriptionEvent::LastKnown(_)) => {
                        let _ = store
                            .update(cx, |store, cx| store.status_unavailable(id, generation, cx));
                    }
                    Err(_) => break,
                }
            }
            let _ = store.update(cx, |store, cx| store.status_unavailable(id, generation, cx));
        });
        interest.task = Some(task);
        cx.emit(ServerGitChanged(id));
    }

    fn status_unavailable(
        &mut self,
        id: ServerGitInterest,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        if let Some(interest) = self
            .statuses
            .get_mut(&id)
            .filter(|interest| interest.generation == generation)
        {
            interest.replica.mark_last_known();
            interest.pull_request = None;
            let target = interest.target;
            self.forget_pull_request_target(target, cx);
            cx.emit(ServerGitChanged(id));
        }
    }
    fn accept_status(
        &mut self,
        id: ServerGitInterest,
        generation: u64,
        epoch: ServerEpoch,
        read: GitReadResult,
        snapshot: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(interest) = self
            .statuses
            .get_mut(&id)
            .filter(|interest| interest.generation == generation)
        else {
            return;
        };
        if read.epoch != epoch {
            return;
        }
        let GitQueryResult::Status(facts) = &read.result else {
            return;
        };
        if interest.epoch == epoch
            && interest.replica.facts().is_some_and(|old| {
                facts.revision < old.revision || (!snapshot && facts.revision == old.revision)
            })
        {
            return;
        }
        // A complete snapshot can confirm the same revision after a publication
        // gap. Keep the old projection until this replacement validates.
        if snapshot || interest.epoch != epoch {
            let mut replacement = GitStatusReplica::new(interest.target, epoch);
            if replacement.accept(read).is_err() {
                return;
            }
            interest.replica = replacement;
            interest.epoch = epoch;
        } else if interest.replica.accept(read).is_err() {
            return;
        }
        interest.pull_request = None;
        interest.read_at = Instant::now();
        let tickets = match interest.replica.scope().zip(interest.replica.facts()) {
            Some((scope, facts)) => {
                self.pull_requests
                    .observe(interest.target, scope.repository, facts, Instant::now())
            }
            None => self.pull_requests.remove(interest.target, Instant::now()),
        }
        .unwrap_or_default();
        self.run_pull_requests(tickets, cx);
        cx.emit(ServerGitChanged(id));
    }

    fn read_inventory(&mut self, id: ServerGitInterest, cx: &mut Context<Self>) {
        let Some(interest) = self.inventories.get_mut(&id) else {
            return;
        };
        interest.task = None;
        interest.replica.require_resync();
        let Some(connection) = self
            .servers
            .get(&interest.workspace.server())
            .filter(|connection| connection.client.is_some())
        else {
            interest.offline = true;
            cx.emit(ServerGitChanged(id));
            return;
        };
        let Some(client) = connection.client.clone() else {
            return;
        };
        let Ok(ticket) = interest.replica.begin(connection.epoch) else {
            return;
        };
        let Some(generation) = interest.generation.checked_add(1) else {
            return;
        };
        interest.generation = generation;
        interest.offline = false;
        let workspace = interest.workspace;
        let task = cx.spawn(async move |store, cx| {
            let mut cursor = None;
            loop {
                let query = GitQuery::Inventory { workspace, cursor };
                let read = client.query(query.clone()).await;
                let progress = store.update(cx, |store, cx| {
                    let interest = store
                        .inventories
                        .get_mut(&id)
                        .filter(|interest| interest.generation == generation)
                        .ok_or(ClientGitFailure::StaleResponse)?;
                    let progress = match read {
                        Ok(read) => interest.replica.accept(ticket, &query, read),
                        Err(error) => {
                            interest.replica.require_resync();
                            Err(error)
                        }
                    };
                    cx.emit(ServerGitChanged(id));
                    progress
                });
                match progress {
                    Ok(Ok(InventoryProgress::More(next))) => cursor = Some(next),
                    _ => break,
                }
            }
        });
        interest.task = Some(task);
        cx.emit(ServerGitChanged(id));
    }
}

/// Read and write entry points over one captured per-server facade. The facade
/// validates epoch, principal, cursors and online admission in client-core.
#[derive(Clone)]
pub(crate) struct ServerGitRequests {
    client: GitClient,
}
impl ServerGitRequests {
    pub(crate) fn server(&self) -> ServerId {
        self.client.server()
    }
    pub(crate) async fn branches(
        &self,
        repository: RepositoryScope,
        cursor: Option<GitPageCursor>,
    ) -> Result<GitBranchPage, ClientGitFailure> {
        match self
            .client
            .query(GitQuery::Branches { repository, cursor })
            .await?
            .result
        {
            GitQueryResult::Branches(page) => Ok(page),
            _ => Err(ClientGitFailure::InvalidResponse),
        }
    }
    pub(crate) async fn preview(
        &self,
        request: WorktreeCreateDraft,
    ) -> Result<GitAssessmentPage, ClientGitFailure> {
        self.assessment(GitQuery::PreviewCreate { request }).await
    }
    pub(crate) async fn assess_remove(
        &self,
        worktree: WorktreeId,
    ) -> Result<GitAssessmentPage, ClientGitFailure> {
        self.assessment(GitQuery::AssessRemove { worktree }).await
    }
    pub(crate) async fn assessment_page(
        &self,
        assessment: GitAssessmentToken,
        cursor: Option<GitPageCursor>,
    ) -> Result<GitAssessmentPage, ClientGitFailure> {
        self.assessment(GitQuery::AssessmentPage { assessment, cursor })
            .await
    }
    async fn assessment(&self, query: GitQuery) -> Result<GitAssessmentPage, ClientGitFailure> {
        match self.client.query(query).await?.result {
            GitQueryResult::Assessment(page) => Ok(*page),
            _ => Err(ClientGitFailure::InvalidResponse),
        }
    }
    pub(crate) async fn submit(
        &self,
        command: GitCommand,
    ) -> Result<TrackedGitOperation, ClientGitFailure> {
        self.client.submit(command).await
    }
    /// Resolve only this original identity. This API never resubmits a command.
    pub(crate) async fn resume(
        &self,
        operation: OperationId,
    ) -> Result<TrackedGitOperation, ClientGitFailure> {
        self.client.resume(operation).await
    }
}

#[cfg(test)]
#[path = "server_git_tests.rs"]
mod tests;
