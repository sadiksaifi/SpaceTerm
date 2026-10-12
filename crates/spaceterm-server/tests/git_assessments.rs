//! Read pipeline through the public service. The actor is an injected boundary fixture.
use spaceterm_core::worktrees::{assessment::*, queries::*};
use spaceterm_core::{
    authority::*, identity::*, operations::*, organization::*, storage::*, worktrees::*,
};
use spaceterm_core::{organization_retained::*, reducer::*};
use spaceterm_server::{
    contracts::ConnectionAccessSnapshot,
    git::{authority::*, host::*, writes::*},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
struct ReadCleanup(GitService);
impl Drop for ReadCleanup {
    fn drop(&mut self) {
        let _ = self.0.quiesce();
    }
}
fn server() -> ServerId {
    ServerId::new([7; 16])
}
fn directory(value: &[u8]) -> ServerDirectory {
    ServerDirectory::new(server(), value.to_vec()).unwrap()
}
fn principal(client: u64) -> AuthenticatedGitPrincipal {
    let client = ClientId::new(server(), client).unwrap();
    AuthenticatedGitPrincipal::from_connection_access(&ConnectionAccessSnapshot {
        server: server(),
        device: DeviceId::new([8; 16]),
        pairing: PairingId::new(server(), 1).unwrap(),
        client,
        binding: ConnectionBinding::new(client, ConnectionGeneration::new(1)),
        epoch: ServerEpoch::new(1),
        reconciled_revision: OrganizationRevision::new(1),
        effects_enabled: true,
    })
    .unwrap()
}
fn organization() -> OrganizationState {
    let workspace = WorkspaceId::new(server(), 1).unwrap();
    let tab = TabId::new(server(), 1).unwrap();
    let pane = PaneId::new(server(), 1).unwrap();
    let directory = ServerDirectory::new(server(), b"/work".to_vec()).unwrap();
    OrganizationState {
        server: server(),
        revision: OrganizationRevision::new(1),
        settings: ServerSettingsRecord {
            worktree_location: Versioned {
                value: WorktreeLocationTemplate::new(
                    "~/Worktrees/{repository}/{branch}".to_owned(),
                )
                .unwrap(),
                version: FieldVersion::new(1),
            },
        },
        workspaces: vec![WorkspaceRecord {
            id: workspace,
            name: Versioned {
                value: WorkspaceName {
                    mode: NameMode::Automatic,
                    display: NameText::new("work".to_owned()).unwrap(),
                },
                version: FieldVersion::new(1),
            },
            pinned_directory: Versioned {
                value: None,
                version: FieldVersion::new(1),
            },
            root_tab: tab,
            creation_ordinal: CreationOrdinal::new(1),
        }],
        tabs: vec![TabRecord {
            id: tab,
            workspace,
            worktree: None,
            root_pane: pane,
            creation_ordinal: CreationOrdinal::new(2),
        }],
        panes: vec![PaneRecord {
            id: pane,
            tab,
            creation_ordinal: CreationOrdinal::new(3),
            creation: PaneCreation::Initial,
            starting_directory: directory,
            confirmed_restoration_directory: Versioned {
                value: None,
                version: FieldVersion::new(1),
            },
        }],
        worktrees: vec![],
        repositories: vec![],
        retained: None,
    }
}

fn versioned_snapshot(
    epoch: ServerEpoch,
    mut state: OrganizationState,
    runtime: Vec<PaneRuntimeRecord>,
) -> OrganizationSnapshot {
    use spaceterm_core::remote_access::RemoteAccessPolicy;
    let mut facts = state
        .retained
        .as_ref()
        .map(|v| v.parts().clone())
        .unwrap_or_else(|| OrganizationRetainedParts {
            server: state.server,
            workspace_names: state
                .workspaces
                .iter()
                .map(|w| {
                    WorkspaceNameBasis::new(
                        w.id,
                        w.name.value.display.clone(),
                        w.name.value.display.clone(),
                    )
                })
                .collect(),
            creation_defaults: state
                .tabs
                .iter()
                .map(|t| {
                    TabCreationDefault::new(
                        t.id,
                        state
                            .panes
                            .iter()
                            .filter(|p| p.tab == t.id)
                            .map(|p| p.id)
                            .collect(),
                    )
                    .unwrap()
                })
                .collect(),
            pane_generations: vec![],
            tab_membership_versions: state
                .tabs
                .iter()
                .map(|t| TabMembershipVersion::new(t.id, FieldVersion::new(1)))
                .collect(),
            repository_inventory_versions: state
                .repositories
                .iter()
                .map(|r| RepositoryInventoryVersion::new(r.id, FieldVersion::new(1)))
                .collect(),
            worktree_inventory: state
                .worktrees
                .iter()
                .map(|w| WorktreeInventoryFacts::new(w.id, true, FieldVersion::new(1)))
                .collect(),
            remote_access: Versioned {
                value: RemoteAccessPolicy::disabled(state.server),
                version: FieldVersion::new(1),
            },
        });
    facts.pane_generations = runtime
        .iter()
        .map(|r| PaneGenerationHighWater::new(r.pane, r.session_generation, r.control_generation))
        .collect();
    state.retained = Some(OrganizationRetainedFacts::new(facts).unwrap());
    OrganizationSnapshot::new(epoch, state, runtime).unwrap()
}

fn snapshot() -> GitAuthoritySnapshot {
    let state = organization();
    let runtime = vec![PaneRuntimeRecord {
        pane: state.panes[0].id,
        session_generation: SessionGeneration::new(0),
        phase: SessionPhase::NotStarted,
        control_generation: ControlGeneration::new(0),
        control: ControlRecord::Unclaimed,
    }];
    let organization = versioned_snapshot(ServerEpoch::new(1), state, runtime);
    let work = organization
        .runtime()
        .iter()
        .map(|r| RuntimeWorkStamp {
            pane: r.pane,
            session: None,
            context_generation: ContextGeneration::new(1),
            work_generation: CloseWorkGeneration::new(1),
            startup_reserved: false,
            confirmed_directory: None,
        })
        .collect();
    GitAuthoritySnapshot {
        organization,
        repositories: vec![],
        home: directory(b"/home"),
        display_directories: vec![],
        runtime: RuntimeWorkSet { panes: work },
    }
}
fn marks() -> IdentityHighWater {
    IdentityHighWater {
        workspace: 1000,
        tab: 1000,
        pane: 1000,
        creation_ordinal: 1000,
        repository: 1000,
        worktree: 1000,
        pairing: 64,
        client: 64,
        operation_stream: 64,
    }
}
struct Actor {
    uncertain: AtomicBool,
    snapshot: Mutex<GitAuthoritySnapshot>,
    authorized: AtomicBool,
    unexpected_effects: AtomicUsize,
    reconciliations: AtomicUsize,
    publications: Mutex<Vec<(u64, OrganizationRevision)>>,
}
fn actor(snapshot: GitAuthoritySnapshot) -> Arc<Actor> {
    Arc::new(Actor {
        uncertain: AtomicBool::new(false),
        snapshot: Mutex::new(snapshot),
        authorized: AtomicBool::new(true),
        unexpected_effects: AtomicUsize::new(0),
        reconciliations: AtomicUsize::new(0),
        publications: Mutex::default(),
    })
}
impl Actor {
    fn effect(&self) {
        self.unexpected_effects.fetch_add(1, Ordering::SeqCst);
    }
}
impl GitOrganizationPort for Actor {
    fn snapshot(&self) -> GitFuture<'_, Result<GitAuthoritySnapshot, GitFailure>> {
        Box::pin(async { Ok(self.snapshot.lock().unwrap().clone()) })
    }
    fn reconcile_inventory(
        &self,
        principal: AuthenticatedGitPrincipal,
        request: GitInventoryReconciliation,
    ) -> GitFuture<'_, Result<GitAuthoritySnapshot, GitFailure>> {
        Box::pin(async move {
            self.authorize(principal).await?;
            if self.uncertain.load(Ordering::SeqCst) {
                return Err(GitFailure::IndeterminateDurability);
            }
            let mut current = self.snapshot.lock().unwrap();
            if current.organization.state().revision != request.basis.organization.state().revision
                || current.display_directories != request.basis.display_directories
            {
                return Err(GitFailure::StaleAssessment);
            }
            let sequence = self.reconciliations.fetch_add(1, Ordering::SeqCst) as u64 + 1;
            let mut high = marks();
            high.repository = high.repository.max(
                current
                    .organization
                    .state()
                    .repositories
                    .iter()
                    .map(|r| r.id.local())
                    .max()
                    .unwrap_or(0),
            );
            high.worktree = high.worktree.max(
                current
                    .organization
                    .state()
                    .worktrees
                    .iter()
                    .map(|r| r.id.local())
                    .max()
                    .unwrap_or(0),
            );
            let mut reducer = OrganizationReducer::from_recovered(
                RetainedOrganization::new(
                    current.organization.state().clone(),
                    high,
                    current.home.clone(),
                ),
                StoreRevision::new(sequence - 1),
                current.organization.epoch(),
                OrganizationLimits::default(),
            )
            .unwrap();
            let version = request.inventory.repository.and_then(|id| {
                current
                    .organization
                    .state()
                    .retained
                    .as_ref()
                    .unwrap()
                    .repository_inventory_versions()
                    .iter()
                    .find(|r| r.id() == id)
                    .map(|r| r.version())
            });
            let mut worktrees = request
                .inventory
                .records
                .iter()
                .map(|w| GitInventoryWorktree {
                    id: w.retained,
                    directory: w.directory.clone(),
                    main: w.main,
                    missing: w.missing,
                    listed: true,
                })
                .collect::<Vec<_>>();
            for old in current.organization.state().worktrees.iter().filter(|w| {
                Some(w.repository) == request.inventory.repository
                    && !request
                        .inventory
                        .records
                        .iter()
                        .any(|r| r.retained == Some(w.id))
            }) {
                worktrees.push(GitInventoryWorktree {
                    id: Some(old.id),
                    directory: old.directory.clone(),
                    main: old.main,
                    missing: true,
                    listed: false,
                });
            }
            let patch = GitInventoryPatch {
                repository: request.inventory.repository,
                directory: request.inventory.main_root.clone(),
                expected_inventory_version: version,
                worktrees,
            };
            let operation = OperationId::new(
                OperationStreamId::new(
                    OperationStreamKey::new(server(), 1).unwrap(),
                    principal.pairing(),
                    principal.client(),
                )
                .unwrap(),
                OperationSequence::new(sequence).unwrap(),
            );
            let context = ReductionContext {
                principal: MutationPrincipal {
                    pairing: principal.pairing(),
                    client: principal.client(),
                },
                operation,
                request_digest: RequestDigest::from_bytes([5; 32]),
                now: ReducerTick::new(sequence),
                facts: ReductionFacts::default(),
            };
            let prepared = reducer
                .prepare_git_inventory_reconciliation(&patch, &request.memberships, &context)
                .unwrap();
            assert!(
                matches!(
                    prepared.resolution(),
                    Resolution::Completed(Outcome::Applied)
                ),
                "{:#?}",
                prepared.resolution()
            );
            let receipt = CommitReceipt {
                store_revision: StoreRevision::new(sequence),
                organization_revision: prepared.resulting_organization_revision(),
            };
            let committed = reducer.apply_committed(prepared, &receipt).unwrap();
            current.organization = reducer.snapshot();
            let repository = current
                .organization
                .state()
                .repositories
                .iter()
                .find(|r| r.directory == request.inventory.main_root)
                .unwrap()
                .id;
            current
                .repositories
                .retain(|r| r.scope().repository != repository);
            current.repositories.push(
                RepositoryLocator::new(RepositoryLocatorParts {
                    scope: RepositoryScope {
                        server: server(),
                        repository,
                    },
                    main_root: request.inventory.main_root,
                    common_directory: request.inventory.common_directory,
                })
                .unwrap(),
            );
            if let Some(lease) = request.inventory.lease {
                lease.retain_committed()?;
            }
            if committed.publication.is_some() {
                self.publications.lock().unwrap().extend([
                    (1, receipt.organization_revision),
                    (2, receipt.organization_revision),
                ]);
            }
            Ok(current.clone())
        })
    }
    fn reconcile_outside_source(
        &self,
        principal: AuthenticatedGitPrincipal,
        request: GitOutsideSourceReconciliation,
    ) -> GitFuture<'_, Result<GitAuthoritySnapshot, GitFailure>> {
        Box::pin(async move {
            self.authorize(principal).await?;
            if self.uncertain.load(Ordering::SeqCst) {
                return Err(GitFailure::IndeterminateDurability);
            }
            let mut current = self.snapshot.lock().unwrap();
            if current.organization != request.basis.organization
                || current.display_directories != request.basis.display_directories
            {
                return Err(GitFailure::StaleAssessment);
            }
            let state = current.organization.state();
            let mut repository = None;
            for edit in &request.memberships {
                let tab = state
                    .tabs
                    .iter()
                    .find(|t| t.id == edit.tab)
                    .ok_or(GitFailure::TargetGone)?;
                let workspace = state
                    .workspaces
                    .iter()
                    .find(|w| w.id == tab.workspace)
                    .ok_or(GitFailure::TargetGone)?;
                let pane = state
                    .panes
                    .iter()
                    .find(|p| p.id == tab.root_pane)
                    .ok_or(GitFailure::TargetGone)?;
                if workspace.pinned_directory.value.is_some()
                    || workspace.root_tab != tab.id
                    || edit.worktree.is_some()
                    || *pane.restoration_directory() != request.directory
                {
                    return Err(GitFailure::StaleAssessment);
                }
                repository = repository.or_else(|| {
                    state
                        .worktrees
                        .iter()
                        .find(|w| Some(w.id) == tab.worktree)
                        .map(|w| w.repository)
                });
            }
            let repository = repository.ok_or(GitFailure::Unchecked)?;
            let sequence = self.reconciliations.fetch_add(1, Ordering::SeqCst) as u64 + 1;
            let mut high = marks();
            high.repository = high.repository.max(
                state
                    .repositories
                    .iter()
                    .map(|r| r.id.local())
                    .max()
                    .unwrap_or(0),
            );
            high.worktree = high.worktree.max(
                state
                    .worktrees
                    .iter()
                    .map(|w| w.id.local())
                    .max()
                    .unwrap_or(0),
            );
            let mut reducer = OrganizationReducer::from_recovered(
                RetainedOrganization::new(state.clone(), high, current.home.clone()),
                StoreRevision::new(sequence - 1),
                current.organization.epoch(),
                OrganizationLimits::default(),
            )
            .unwrap();
            let context = ReductionContext {
                principal: MutationPrincipal {
                    pairing: principal.pairing(),
                    client: principal.client(),
                },
                operation: OperationId::new(
                    OperationStreamId::new(
                        OperationStreamKey::new(server(), 1).unwrap(),
                        principal.pairing(),
                        principal.client(),
                    )
                    .unwrap(),
                    OperationSequence::new(sequence).unwrap(),
                ),
                request_digest: RequestDigest::from_bytes([5; 32]),
                now: ReducerTick::new(sequence),
                facts: ReductionFacts::default(),
            };
            let patch = WorktreeOrganizationPatch {
                binding: context.binding(),
                scope: RepositoryScope {
                    server: server(),
                    repository,
                },
                expected_inventory_version: state
                    .retained
                    .as_ref()
                    .unwrap()
                    .repository_inventory_versions()
                    .iter()
                    .find(|v| v.id() == repository)
                    .map(|v| v.version()),
                repositories: vec![],
                worktrees: vec![],
                memberships: request.memberships,
                settlement: None,
            };
            let prepared = reducer
                .prepare_event(&OrganizationEvent::WorktreeOrganizationChanged(Box::new(
                    patch,
                )))
                .unwrap();
            assert!(matches!(
                prepared.resolution(),
                Resolution::Completed(Outcome::Applied)
            ));
            let receipt = CommitReceipt {
                store_revision: StoreRevision::new(sequence),
                organization_revision: prepared.resulting_organization_revision(),
            };
            let committed = reducer.apply_committed(prepared, &receipt).unwrap();
            current.organization = reducer.snapshot();
            if committed.publication.is_some() {
                self.publications.lock().unwrap().extend([
                    (1, receipt.organization_revision),
                    (2, receipt.organization_revision),
                ]);
            }
            Ok(current.clone())
        })
    }
    fn authorize(&self, _: AuthenticatedGitPrincipal) -> GitFuture<'_, Result<(), GitFailure>> {
        Box::pin(async {
            if self.authorized.load(Ordering::SeqCst) {
                Ok(())
            } else {
                Err(GitFailure::Unauthorized)
            }
        })
    }
    fn lookup<'a>(
        &'a self,
        _: AuthenticatedGitPrincipal,
        id: OperationId,
        c: &'a GitCommand,
    ) -> GitFuture<'a, Result<GitOperationLookup, GitFailure>> {
        let _ = (id, c);
        Box::pin(async { Err(GitFailure::Busy) })
    }
    fn reserve_confirmed(
        &self,
        _: ConfirmedGitAdmission,
    ) -> GitFuture<'_, Result<GitReservation, GitFailure>> {
        self.effect();
        Box::pin(async { Err(GitFailure::Busy) })
    }
    fn persist_prepared<'a>(&'a self, _: &'a GitReservation) -> GitFuture<'a, CommitDisposition> {
        self.effect();
        Box::pin(async { CommitDisposition::NotApplied(StoreFailure::Unavailable) })
    }
    fn barrier_and_revalidate<'a>(
        &'a self,
        _: &'a GitReservation,
    ) -> GitFuture<'a, Result<(), GitFailure>> {
        self.effect();
        Box::pin(async { Err(GitFailure::Busy) })
    }
    fn persist_started<'a>(&'a self, _: &'a GitReservation) -> GitFuture<'a, CommitDisposition> {
        self.effect();
        Box::pin(async { CommitDisposition::NotApplied(StoreFailure::Unavailable) })
    }
    fn retire_affected<'a>(
        &'a self,
        _: &'a GitReservation,
    ) -> GitFuture<'a, Result<GitRetirementReport, GitFailure>> {
        self.effect();
        Box::pin(async { Err(GitFailure::Busy) })
    }
    fn complete<'a>(
        &'a self,
        _: &'a GitReservation,
        _: &'a GitCompletionPlan,
    ) -> GitFuture<'a, GitCompletionCommit> {
        self.effect();
        Box::pin(async {
            GitCompletionCommit {
                disposition: CommitDisposition::NotApplied(StoreFailure::Unavailable),
                outcome: None,
            }
        })
    }
    fn release(&self, _: GitReservation) -> GitFuture<'_, ()> {
        self.effect();
        Box::pin(async {})
    }
    fn recover_reservation<'a>(
        &'a self,
        _: &'a GitWriteIntent,
    ) -> GitFuture<'a, Result<GitReservation, GitFailure>> {
        self.effect();
        Box::pin(async { Err(GitFailure::Busy) })
    }
    fn close_mutation_admission(&self) -> GitFuture<'_, ()> {
        self.effect();
        Box::pin(async {})
    }
}

struct Host {
    reads: AtomicUsize,
}
impl ServerGitHost for Host {
    fn status(
        &self,
        directory: &ServerDirectory,
        _: &GitAuthoritySnapshot,
        _: &GitReadCancellation,
    ) -> Result<HostGitStatus, GitFailure> {
        assert_eq!(directory.bytes(), b"/work");
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(HostGitStatus {
            inventory: None,
            facts: GitStatusFacts {
                revision: std::num::NonZeroU64::new(1).unwrap(),
                branch: None,
                changes: vec![],
                dirty: false,
                unchecked: false,
                pull_request: None,
                upstream: None,
                operation: None,
                counts: None,
            },
        })
    }
    fn prepare_write(
        &self,
        _: HostGitWriteRequest,
        _: &GitReadCancellation,
    ) -> Result<Box<dyn PreparedServerGitWrite>, GitFailure> {
        Err(GitFailure::Unchecked)
    }
    fn reconcile(
        &self,
        _: &GitWriteIntent,
        _: &GitReadCancellation,
    ) -> Result<HostRecoveryObservation, GitFailure> {
        Err(GitFailure::Unchecked)
    }
    fn shutdown(&self) -> Result<(), GitFailure> {
        Ok(())
    }
}
#[tokio::test]
async fn status_resolves_the_committed_root_pane_directory_without_a_client_path() {
    let actor = actor(snapshot());
    let host = Arc::new(Host {
        reads: AtomicUsize::new(0),
    });
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        host.clone(),
        actor.clone(),
        GitLimits::default(),
    )
    .unwrap();
    let _cleanup = ReadCleanup(service.clone());
    service.recover(vec![]).await.unwrap();
    let result = service
        .query(
            principal(1),
            GitQuery::Status {
                target: GitStatusTarget::Workspace(WorkspaceId::new(server(), 1).unwrap()),
            },
        )
        .await;
    assert!(matches!(
        result,
        Ok(GitQueryResult::Status(GitStatusFacts {
            unchecked: false,
            ..
        }))
    ));
    assert_eq!(host.reads.load(Ordering::SeqCst), 1);
    assert_eq!(actor.unexpected_effects.load(Ordering::SeqCst), 0);
    service.shutdown().await.unwrap();
}

struct NativeFixture {
    root: std::path::PathBuf,
    host: Arc<NativeServerGitHost>,
}
impl NativeFixture {
    fn new(name: &str) -> Self {
        use spaceterm_host::{
            git::{UnixRepositoryProgramRunner, tools},
            server_directories::native::NativeDirectoryProvider,
        };
        let root =
            std::env::temp_dir().join(format!("spaceterm-git-read-{name}-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::create_dir(root.join("repository")).unwrap();
        git(&root.join("repository"), &["init", "-b", "main"]);
        git(
            &root.join("repository"),
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@invalid",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-m",
                "initial",
            ],
        );
        let version = tools::parse_git_version(
            &std::process::Command::new("/usr/bin/git")
                .arg("--version")
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let provider = Arc::new(NativeDirectoryProvider::new(server(), root.clone()).unwrap());
        let host = Arc::new(NativeServerGitHost::new(
            Arc::new(UnixRepositoryProgramRunner::new()),
            "/usr/bin/git".into(),
            root.clone(),
            version,
            provider,
        ));
        Self { root, host }
    }
    fn main(&self) -> std::path::PathBuf {
        self.root.join("repository")
    }
}
impl Drop for NativeFixture {
    fn drop(&mut self) {
        self.host.shutdown().unwrap();
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}
fn git(root: &std::path::Path, args: &[&str]) {
    assert!(
        std::process::Command::new("/usr/bin/git")
            .current_dir(root)
            .args(args)
            .env_clear()
            .env("HOME", root.parent().unwrap())
            .env("PATH", "/usr/bin:/bin")
            .env("LC_ALL", "C")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    );
}
#[test]
fn native_status_supplies_aggregate_counts_and_credential_free_pull_request_facts() {
    let fixture = NativeFixture::new("status-counts");
    for index in 0..25 {
        std::fs::write(fixture.main().join(format!("change-{index}")), "test").unwrap();
    }
    git(
        &fixture.main(),
        &[
            "remote",
            "add",
            "upstream",
            "https://secret@github.com/base/project.git",
        ],
    );
    git(
        &fixture.main(),
        &[
            "remote",
            "add",
            "origin",
            "git@ssh.github.com:fork/project.git",
        ],
    );
    git(&fixture.main(), &["config", "remote.pushDefault", "origin"]);
    let directory = directory(fixture.main().as_os_str().as_encoded_bytes());
    let observed = fixture
        .host
        .status(&directory, &snapshot(), &GitReadCancellation::default())
        .unwrap();
    assert_eq!(
        observed.facts.counts.unwrap().total,
        GitChangeTotal::Exact(25)
    );
    assert_eq!(observed.facts.changes.len(), 20);
    let pull_request = observed.facts.pull_request.unwrap();
    assert_eq!(
        (
            pull_request.host(),
            pull_request.owner(),
            pull_request.repository(),
            pull_request.head_owner()
        ),
        ("github.com", "base", "project", Some("fork"))
    );
}

#[tokio::test]
async fn inventory_allocates_through_the_reducer_and_publishes_membership_in_one_commit() {
    let fixture = NativeFixture::new("inventory-membership");
    let mut initial = snapshot();
    let mut state = initial.organization.state().clone();
    state.panes[0].starting_directory = directory(fixture.main().as_os_str().as_encoded_bytes());
    initial.organization = OrganizationSnapshot::new(
        initial.organization.epoch(),
        state,
        initial.organization.runtime().to_vec(),
    )
    .unwrap();
    let actor = actor(initial);
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        fixture.host.clone(),
        actor.clone(),
        GitLimits::default(),
    )
    .unwrap();
    let _cleanup = ReadCleanup(service.clone());
    service.recover(vec![]).await.unwrap();
    let GitQueryResult::Inventory(page) = service
        .query(
            principal(1),
            GitQuery::Inventory {
                workspace: WorkspaceId::new(server(), 1).unwrap(),
                cursor: None,
            },
        )
        .await
        .unwrap()
    else {
        panic!("inventory")
    };
    {
        let committed = actor.snapshot.lock().unwrap();
        assert_eq!(page.records.len(), 1);
        assert_eq!(
            page.records[0].worktree.local(),
            1001,
            "the reducer owns the high-water allocator"
        );
        assert_eq!(
            committed.organization.state().tabs[0].worktree,
            Some(page.records[0].worktree)
        );
        assert_eq!(
            committed.organization.state().workspaces[0].root_tab,
            TabId::new(server(), 1).unwrap()
        );
        assert_eq!(
            actor.publications.lock().unwrap().as_slice(),
            &[(1, page.version), (2, page.version)],
            "the inactive client receives the same committed revision"
        );
    }
    service.shutdown().await.unwrap();
}

struct ControlledHost {
    host: Arc<NativeServerGitHost>,
    block: AtomicBool,
    block_from: AtomicUsize,
    fail: AtomicBool,
    entered: AtomicUsize,
    finished: AtomicUsize,
}
impl ControlledHost {
    fn new(host: Arc<NativeServerGitHost>) -> Arc<Self> {
        Arc::new(Self {
            host,
            block: AtomicBool::new(false),
            block_from: AtomicUsize::new(0),
            fail: AtomicBool::new(false),
            entered: AtomicUsize::new(0),
            finished: AtomicUsize::new(0),
        })
    }
}
impl ServerGitHost for ControlledHost {
    fn discover_source(
        &self,
        directory: &ServerDirectory,
        basis: &GitAuthoritySnapshot,
        cancellation: &GitReadCancellation,
    ) -> Result<Option<HostGitSource>, GitFailure> {
        self.host.discover_source(directory, basis, cancellation)
    }
    fn containment(
        &self,
        directory: &ServerDirectory,
        root: &ServerDirectory,
    ) -> spaceterm_core::server_directories::DirectoryContainment {
        self.host.containment(directory, root)
    }
    fn status(
        &self,
        directory: &ServerDirectory,
        basis: &GitAuthoritySnapshot,
        cancellation: &GitReadCancellation,
    ) -> Result<HostGitStatus, GitFailure> {
        let read = self.entered.fetch_add(1, Ordering::SeqCst) + 1;
        while (self.block.load(Ordering::SeqCst)
            || (self.block_from.load(Ordering::SeqCst) != 0
                && read >= self.block_from.load(Ordering::SeqCst)))
            && !cancellation.is_cancelled()
        {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let result = if cancellation.is_cancelled() {
            Err(GitFailure::CancelledRead)
        } else if self.fail.load(Ordering::SeqCst) {
            Err(GitFailure::Unchecked)
        } else {
            self.host.status(directory, basis, cancellation)
        };
        self.finished.fetch_add(1, Ordering::SeqCst);
        result
    }
    fn inventory(
        &self,
        directory: &ServerDirectory,
        basis: &GitAuthoritySnapshot,
        cancellation: &GitReadCancellation,
    ) -> Result<Option<HostGitInventory>, GitFailure> {
        self.host.inventory(directory, basis, cancellation)
    }
    fn branches(
        &self,
        repository: &RepositoryLocator,
        cancellation: &GitReadCancellation,
    ) -> Result<Vec<GitBranchRecord>, GitFailure> {
        self.host.branches(repository, cancellation)
    }
    fn prepare_write(
        &self,
        _: HostGitWriteRequest,
        _: &GitReadCancellation,
    ) -> Result<Box<dyn PreparedServerGitWrite>, GitFailure> {
        Err(GitFailure::Unchecked)
    }
    fn reconcile(
        &self,
        _: &GitWriteIntent,
        _: &GitReadCancellation,
    ) -> Result<HostRecoveryObservation, GitFailure> {
        Err(GitFailure::Unchecked)
    }
    fn shutdown(&self) -> Result<(), GitFailure> {
        self.host.shutdown()
    }
}
fn native_actor(fixture: &NativeFixture) -> Arc<Actor> {
    let mut initial = snapshot();
    let mut state = initial.organization.state().clone();
    state.panes[0].starting_directory = directory(fixture.main().as_os_str().as_encoded_bytes());
    initial.organization = OrganizationSnapshot::new(
        initial.organization.epoch(),
        state,
        initial.organization.runtime().to_vec(),
    )
    .unwrap();
    actor(initial)
}
fn workspace_target() -> GitStatusTarget {
    GitStatusTarget::Workspace(WorkspaceId::new(server(), 1).unwrap())
}
async fn wait_count(counter: &AtomicUsize, count: usize) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while counter.load(Ordering::SeqCst) < count {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn failed_status_refresh_keeps_principal_bound_last_known_facts() {
    let fixture = NativeFixture::new("last-known");
    let actor = native_actor(&fixture);
    let host = ControlledHost::new(fixture.host.clone());
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        host.clone(),
        actor.clone(),
        GitLimits::default(),
    )
    .unwrap();
    let _cleanup = ReadCleanup(service.clone());
    service.recover(vec![]).await.unwrap();
    let GitQueryResult::Status(first) = service
        .query(
            principal(1),
            GitQuery::Status {
                target: workspace_target(),
            },
        )
        .await
        .unwrap()
    else {
        panic!("status")
    };
    host.fail.store(true, Ordering::SeqCst);
    let GitQueryResult::Status(last_known) = service
        .query(
            principal(1),
            GitQuery::Status {
                target: workspace_target(),
            },
        )
        .await
        .unwrap()
    else {
        panic!("status")
    };
    assert!(last_known.unchecked);
    assert_eq!(last_known.branch, first.branch);
    assert!(last_known.revision > first.revision);
    assert_eq!(
        service
            .query(
                principal(2),
                GitQuery::Status {
                    target: workspace_target()
                }
            )
            .await,
        Err(GitFailure::Unchecked),
        "another client cannot borrow the first client's observation"
    );
    actor.authorized.store(false, Ordering::SeqCst);
    assert_eq!(
        service
            .query(
                principal(1),
                GitQuery::Status {
                    target: workspace_target()
                }
            )
            .await,
        Err(GitFailure::Unauthorized)
    );
    service.shutdown().await.unwrap();
}
#[tokio::test]
async fn subscriptions_wait_for_a_new_revision_and_coalesce_repeated_triggers() {
    let fixture = NativeFixture::new("status-subscription");
    let actor = native_actor(&fixture);
    let host = ControlledHost::new(fixture.host.clone());
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        host.clone(),
        actor.clone(),
        GitLimits::default(),
    )
    .unwrap();
    let _cleanup = ReadCleanup(service.clone());
    service.recover(vec![]).await.unwrap();
    let mut subscription = service
        .subscribe_status(principal(1), workspace_target())
        .await
        .unwrap();
    let first = subscription.changed().await.unwrap().result.unwrap();
    host.block.store(true, Ordering::SeqCst);
    service.refresh_statuses().unwrap();
    wait_count(&host.entered, 2).await;
    for _ in 0..20 {
        service.refresh_statuses().unwrap();
    }
    host.block.store(false, Ordering::SeqCst);
    let next = subscription.changed().await.unwrap().result.unwrap();
    assert!(next.revision > first.revision);
    wait_count(&host.finished, 3).await;
    assert_eq!(
        host.entered.load(Ordering::SeqCst),
        3,
        "one follow-up, not one queued job per trigger"
    );
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn branch_pages_round_trip_both_majors_and_cannot_cross_principals_or_repositories() {
    let fixture = NativeFixture::new("branch-pages");
    let names = (0..140)
        .map(|index| {
            format!(
                "b{index}/{}/{}/{}/{}",
                "a".repeat(200),
                "b".repeat(200),
                "c".repeat(200),
                "d".repeat(200)
            )
        })
        .collect::<Vec<_>>();
    for name in &names {
        git(&fixture.main(), &["branch", name]);
    }
    let actor = native_actor(&fixture);
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        fixture.host.clone(),
        actor.clone(),
        GitLimits::default(),
    )
    .unwrap();
    let _cleanup = ReadCleanup(service.clone());
    service.recover(vec![]).await.unwrap();
    service
        .query(
            principal(1),
            GitQuery::Inventory {
                workspace: WorkspaceId::new(server(), 1).unwrap(),
                cursor: None,
            },
        )
        .await
        .unwrap();
    let scope = actor.snapshot.lock().unwrap().repositories[0].scope();
    let mut cursor = None;
    let mut first_cursor = None;
    let mut collected = Vec::new();
    let mut pages = 0;
    loop {
        let response = service
            .query_request(
                principal(1),
                GitQueryRequest::new(
                    GitCorrelation::new(pages + 1, 1).unwrap(),
                    GitQuery::Branches {
                        repository: scope,
                        cursor: cursor.clone(),
                    },
                    None,
                    false,
                )
                .unwrap(),
            )
            .await;
        for major in [1, 2] {
            let payload =
                spaceterm_protocol::convert::git::encode_response(major, &response, server())
                    .unwrap();
            assert!(payload.encode().unwrap().len() < 64 * 1024);
            assert_eq!(
                spaceterm_protocol::convert::git::decode_response(&payload, server()).unwrap(),
                response
            );
        }
        let GitQueryResult::Branches(page) = response.result.unwrap() else {
            panic!("branches")
        };
        assert!(page.records.len() <= MAX_GIT_PAGE_RECORDS);
        if let Some(next) = &page.next {
            first_cursor.get_or_insert(next.clone());
            assert_eq!(
                service
                    .query(
                        principal(2),
                        GitQuery::Branches {
                            repository: scope,
                            cursor: Some(next.clone())
                        }
                    )
                    .await,
                Err(GitFailure::Unauthorized)
            );
            assert_eq!(
                service
                    .query(
                        principal(1),
                        GitQuery::Branches {
                            repository: RepositoryScope {
                                repository: RepositoryId::new(
                                    server(),
                                    scope.repository.local() + 1
                                )
                                .unwrap(),
                                ..scope
                            },
                            cursor: Some(next.clone())
                        }
                    )
                    .await,
                Err(GitFailure::StaleAssessment)
            );
        }
        collected.extend(
            page.records
                .into_iter()
                .map(|record| record.name.as_str().to_owned()),
        );
        pages += 1;
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    assert!(
        pages >= 3,
        "byte bounds reduce long-name pages below the record ceiling"
    );
    assert_eq!(collected.len(), 141);
    assert!(names.iter().all(|name| collected.contains(name)));
    service
        .query(
            principal(1),
            GitQuery::Branches {
                repository: scope,
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        service
            .query(
                principal(1),
                GitQuery::Branches {
                    repository: scope,
                    cursor: first_cursor
                }
            )
            .await,
        Err(GitFailure::StaleAssessment),
        "a fresh observation replaces the old branch-page base"
    );
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn native_removal_query_binds_risks_all_affected_work_and_nonstarting_fallback() {
    let fixture = NativeFixture::new("removal-assessment");
    let linked = fixture.root.join("linked");
    git(
        &fixture.main(),
        &["worktree", "add", "-b", "topic", linked.to_str().unwrap()],
    );
    std::fs::write(linked.join("untracked"), "fixture").unwrap();
    let actor = native_actor(&fixture);
    {
        let mut current = actor.snapshot.lock().unwrap();
        let mut state = current.organization.state().clone();
        state.panes[0].starting_directory = directory(linked.as_os_str().as_encoded_bytes());
        current.organization = OrganizationSnapshot::new(
            current.organization.epoch(),
            state,
            current.organization.runtime().to_vec(),
        )
        .unwrap();
    }
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        fixture.host.clone(),
        actor.clone(),
        GitLimits::default(),
    )
    .unwrap();
    let _cleanup = ReadCleanup(service.clone());
    service.recover(vec![]).await.unwrap();
    service
        .query(
            principal(1),
            GitQuery::Inventory {
                workspace: WorkspaceId::new(server(), 1).unwrap(),
                cursor: None,
            },
        )
        .await
        .unwrap();
    let worktree = actor
        .snapshot
        .lock()
        .unwrap()
        .organization
        .state()
        .worktrees
        .iter()
        .find(|w| !w.main)
        .unwrap()
        .id;
    let GitQueryResult::Assessment(page) = service
        .query(principal(1), GitQuery::AssessRemove { worktree })
        .await
        .unwrap()
    else {
        panic!("assessment")
    };
    assert_eq!(
        page.removal.as_ref().unwrap().risk,
        ApprovedRemoval::DiscardChanges
    );
    assert_eq!(
        page.removal
            .as_ref()
            .unwrap()
            .branch
            .as_ref()
            .unwrap()
            .as_str(),
        "topic"
    );
    assert_eq!(page.tabs.len(), 1);
    assert_eq!(page.panes.len(), 1);
    assert_eq!(page.fallbacks.len(), 1);
    assert_eq!(
        page.fallbacks[0].directory.bytes(),
        fixture.main().as_os_str().as_encoded_bytes()
    );
    let scope = actor.snapshot.lock().unwrap().repositories[0].scope();
    for major in [1, 2] {
        let response = GitQueryResponse {
            correlation: GitCorrelation::new(1, 1).unwrap(),
            scope: Some(scope),
            result: Ok(GitQueryResult::Assessment(page.clone())),
        };
        let payload =
            spaceterm_protocol::convert::git::encode_response(major, &response, server()).unwrap();
        assert_eq!(
            spaceterm_protocol::convert::git::decode_response(&payload, server()).unwrap(),
            response
        );
    }
    assert_eq!(
        service
            .query(
                principal(2),
                GitQuery::AssessmentPage {
                    assessment: page.token.clone(),
                    cursor: None
                }
            )
            .await,
        Err(GitFailure::Unauthorized)
    );
    assert!(
        linked.exists(),
        "assessment does not remove or retire anything"
    );
    assert_eq!(actor.unexpected_effects.load(Ordering::SeqCst), 0);
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn creation_query_uses_native_server_preview_without_a_repository_write() {
    let fixture = NativeFixture::new("creation-assessment");
    git(&fixture.main(), &["branch", "waiting"]);
    let actor = native_actor(&fixture);
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        fixture.host.clone(),
        actor.clone(),
        GitLimits::default(),
    )
    .unwrap();
    let _cleanup = ReadCleanup(service.clone());
    service.recover(vec![]).await.unwrap();
    service
        .query(
            principal(1),
            GitQuery::Inventory {
                workspace: WorkspaceId::new(server(), 1).unwrap(),
                cursor: None,
            },
        )
        .await
        .unwrap();
    let scope = actor.snapshot.lock().unwrap().repositories[0].scope();
    let target = fixture.root.join("new");
    let draft = WorktreeCreateDraft::new(
        scope,
        WorktreeBranchChoice::Existing {
            name: BranchName::new("waiting".into()).unwrap(),
        },
        WorktreeLocationChoice::Explicit(directory(target.as_os_str().as_encoded_bytes())),
    )
    .unwrap();
    let GitQueryResult::Assessment(page) = service
        .query(principal(1), GitQuery::PreviewCreate { request: draft })
        .await
        .unwrap()
    else {
        panic!("assessment")
    };
    assert_eq!(page.token.kind(), GitAssessmentKind::Create);
    assert_eq!(
        page.creation_preview
            .as_ref()
            .unwrap()
            .parts()
            .directory
            .bytes(),
        target.as_os_str().as_encoded_bytes()
    );
    assert!(page.removal.is_none());
    assert!(!target.exists());
    assert_eq!(actor.unexpected_effects.load(Ordering::SeqCst), 0);
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn late_status_cannot_replace_a_changed_directory_binding() {
    let fixture = NativeFixture::new("late-status");
    let actor = native_actor(&fixture);
    let host = ControlledHost::new(fixture.host.clone());
    host.block.store(true, Ordering::SeqCst);
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        host.clone(),
        actor.clone(),
        GitLimits::default(),
    )
    .unwrap();
    let _cleanup = ReadCleanup(service.clone());
    service.recover(vec![]).await.unwrap();
    let query_service = service.clone();
    let query = tokio::spawn(async move {
        query_service
            .query(
                principal(1),
                GitQuery::Status {
                    target: workspace_target(),
                },
            )
            .await
    });
    wait_count(&host.entered, 1).await;
    {
        let mut current = actor.snapshot.lock().unwrap();
        let mut state = current.organization.state().clone();
        state.panes[0].starting_directory = directory(fixture.root.as_os_str().as_encoded_bytes());
        current.organization = OrganizationSnapshot::new(
            current.organization.epoch(),
            state,
            current.organization.runtime().to_vec(),
        )
        .unwrap();
    }
    host.block.store(false, Ordering::SeqCst);
    assert_eq!(query.await.unwrap(), Err(GitFailure::StaleAssessment));
    assert!(actor.publications.lock().unwrap().is_empty());
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_cancels_and_joins_a_read_after_its_caller_disappears() {
    let fixture = NativeFixture::new("dropped-read");
    let actor = native_actor(&fixture);
    let host = ControlledHost::new(fixture.host.clone());
    host.block.store(true, Ordering::SeqCst);
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        host.clone(),
        actor,
        GitLimits::default(),
    )
    .unwrap();
    let _cleanup = ReadCleanup(service.clone());
    service.recover(vec![]).await.unwrap();
    let query_service = service.clone();
    let query = tokio::spawn(async move {
        query_service
            .query(
                principal(1),
                GitQuery::Status {
                    target: workspace_target(),
                },
            )
            .await
    });
    wait_count(&host.entered, 1).await;
    query.abort();
    tokio::time::timeout(std::time::Duration::from_secs(5), service.shutdown())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        host.finished.load(Ordering::SeqCst),
        1,
        "the service, not the caller, owns the worker"
    );
}

#[tokio::test]
async fn retained_bare_main_is_listed_without_assigning_it_to_a_tab() {
    let fixture = NativeFixture::new("bare-inventory");
    let bare = fixture.root.join("bare.git");
    let linked = fixture.root.join("bare-linked");
    git(
        &fixture.main(),
        &[
            "clone",
            "--bare",
            fixture.main().to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    git(
        &bare,
        &["worktree", "add", "-b", "topic", linked.to_str().unwrap()],
    );
    let actor = native_actor(&fixture);
    {
        let mut current = actor.snapshot.lock().unwrap();
        let mut state = current.organization.state().clone();
        state.panes[0].starting_directory = directory(linked.as_os_str().as_encoded_bytes());
        current.organization = OrganizationSnapshot::new(
            current.organization.epoch(),
            state,
            current.organization.runtime().to_vec(),
        )
        .unwrap();
    }
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        fixture.host.clone(),
        actor.clone(),
        GitLimits::default(),
    )
    .unwrap();
    let _cleanup = ReadCleanup(service.clone());
    service.recover(vec![]).await.unwrap();
    service
        .query(
            principal(1),
            GitQuery::Inventory {
                workspace: WorkspaceId::new(server(), 1).unwrap(),
                cursor: None,
            },
        )
        .await
        .unwrap();
    {
        let mut current = actor.snapshot.lock().unwrap();
        let mut state = current.organization.state().clone();
        state.panes[0].starting_directory = directory(bare.as_os_str().as_encoded_bytes());
        current.organization = OrganizationSnapshot::new(
            current.organization.epoch(),
            state,
            current.organization.runtime().to_vec(),
        )
        .unwrap();
    }
    let GitQueryResult::Inventory(page) = service
        .query(
            principal(1),
            GitQuery::Inventory {
                workspace: WorkspaceId::new(server(), 1).unwrap(),
                cursor: None,
            },
        )
        .await
        .unwrap()
    else {
        panic!("inventory")
    };
    assert_eq!(page.records.len(), 2);
    assert!(
        actor.snapshot.lock().unwrap().organization.state().tabs[0]
            .worktree
            .is_none()
    );
    service.shutdown().await.unwrap();
}

fn with_panes(mut snapshot: GitAuthoritySnapshot, count: u64) -> GitAuthoritySnapshot {
    let mut state = snapshot.organization.state().clone();
    for index in 2..=count {
        let mut pane = state.panes[0].clone();
        pane.id = PaneId::new(server(), index).unwrap();
        pane.creation_ordinal = CreationOrdinal::new(index + 2);
        pane.creation = PaneCreation::SplitOf {
            source: state.panes[0].id,
            axis: SplitAxis::Horizontal,
            edge: SplitEdge::After,
        };
        state.panes.push(pane);
    }
    state.retained = None;
    let runtime = state
        .panes
        .iter()
        .map(|p| PaneRuntimeRecord {
            pane: p.id,
            session_generation: SessionGeneration::new(0),
            phase: SessionPhase::NotStarted,
            control_generation: ControlGeneration::new(0),
            control: ControlRecord::Unclaimed,
        })
        .collect();
    snapshot.organization = versioned_snapshot(snapshot.organization.epoch(), state, runtime);
    snapshot.runtime.panes = snapshot
        .organization
        .runtime()
        .iter()
        .map(|r| RuntimeWorkStamp {
            pane: r.pane,
            session: None,
            context_generation: ContextGeneration::new(1),
            work_generation: CloseWorkGeneration::new(1),
            startup_reserved: false,
            confirmed_directory: None,
        })
        .collect();
    snapshot
}
#[tokio::test]
async fn read_admission_enforces_the_principal_quota_without_cancelling_other_clients() {
    let fixture = NativeFixture::new("read-quota");
    let initial = with_panes(native_actor(&fixture).snapshot.lock().unwrap().clone(), 5);
    let actor = actor(initial);
    let host = ControlledHost::new(fixture.host.clone());
    host.block.store(true, Ordering::SeqCst);
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        host.clone(),
        actor,
        GitLimits::default(),
    )
    .unwrap();
    let _cleanup = ReadCleanup(service.clone());
    service.recover(vec![]).await.unwrap();
    let mut callers = Vec::new();
    for index in 1..=4 {
        let service = service.clone();
        callers.push(tokio::spawn(async move {
            service
                .query(
                    principal(1),
                    GitQuery::Status {
                        target: GitStatusTarget::Pane(PaneId::new(server(), index).unwrap()),
                    },
                )
                .await
        }));
    }
    wait_count(&host.entered, 4).await;
    assert_eq!(
        service
            .query(
                principal(1),
                GitQuery::Status {
                    target: GitStatusTarget::Pane(PaneId::new(server(), 5).unwrap())
                }
            )
            .await,
        Err(GitFailure::Capacity)
    );
    let other_service = service.clone();
    let other = tokio::spawn(async move {
        other_service
            .query(
                principal(2),
                GitQuery::Status {
                    target: workspace_target(),
                },
            )
            .await
    });
    wait_count(&host.entered, 5).await;
    service.shutdown().await.unwrap();
    for caller in callers {
        assert!(caller.await.unwrap().is_err());
    }
    assert!(other.await.unwrap().is_err());
    assert_eq!(host.finished.load(Ordering::SeqCst), 5);
}

#[tokio::test]
async fn assessment_pages_preserve_the_complete_scope_and_token_binding() {
    let fixture = NativeFixture::new("assessment-pages");
    let linked = fixture.root.join("linked");
    git(
        &fixture.main(),
        &["worktree", "add", "-b", "topic", linked.to_str().unwrap()],
    );
    let mut initial = native_actor(&fixture).snapshot.lock().unwrap().clone();
    let mut state = initial.organization.state().clone();
    state.panes[0].starting_directory = directory(linked.as_os_str().as_encoded_bytes());
    initial.organization = OrganizationSnapshot::new(
        initial.organization.epoch(),
        state,
        initial.organization.runtime().to_vec(),
    )
    .unwrap();
    let actor = actor(with_panes(initial, 130));
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        fixture.host.clone(),
        actor.clone(),
        GitLimits::default(),
    )
    .unwrap();
    let _cleanup = ReadCleanup(service.clone());
    service.recover(vec![]).await.unwrap();
    service
        .query(
            principal(1),
            GitQuery::Inventory {
                workspace: WorkspaceId::new(server(), 1).unwrap(),
                cursor: None,
            },
        )
        .await
        .unwrap();
    let worktree = actor
        .snapshot
        .lock()
        .unwrap()
        .organization
        .state()
        .worktrees
        .iter()
        .find(|w| !w.main)
        .unwrap()
        .id;
    let GitQueryResult::Assessment(first) = service
        .query(principal(1), GitQuery::AssessRemove { worktree })
        .await
        .unwrap()
    else {
        panic!("assessment")
    };
    assert_eq!(
        first.tabs.len() + first.panes.len() + first.fallbacks.len(),
        MAX_GIT_PAGE_RECORDS
    );
    let mut panes = first.panes.clone();
    let mut cursor = first.next.clone();
    while let Some(next) = cursor {
        assert_eq!(
            service
                .query(
                    principal(2),
                    GitQuery::AssessmentPage {
                        assessment: first.token.clone(),
                        cursor: Some(next.clone())
                    }
                )
                .await,
            Err(GitFailure::Unauthorized)
        );
        let GitQueryResult::Assessment(page) = service
            .query(
                principal(1),
                GitQuery::AssessmentPage {
                    assessment: first.token.clone(),
                    cursor: Some(next),
                },
            )
            .await
            .unwrap()
        else {
            panic!("assessment")
        };
        assert_eq!(page.removal, first.removal);
        panes.extend(page.panes);
        cursor = page.next;
    }
    assert_eq!(panes.len(), 130);
    let ids = panes
        .iter()
        .map(|p| p.pane)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        ids.len(),
        130,
        "authorization pages do not truncate or duplicate hierarchy"
    );
    let mut wrong = first.token.parts().clone();
    wrong.epoch = ServerEpoch::new(2);
    assert_eq!(
        service
            .query(
                principal(1),
                GitQuery::AssessmentPage {
                    assessment: GitAssessmentToken::new(wrong).unwrap(),
                    cursor: None
                }
            )
            .await,
        Err(GitFailure::StaleAssessment)
    );
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn inventory_refresh_moves_the_root_without_an_active_subscription() {
    let fixture = NativeFixture::new("inactive-root-refresh");
    let actor = native_actor(&fixture);
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        fixture.host.clone(),
        actor.clone(),
        GitLimits::default(),
    )
    .unwrap();
    let _cleanup = ReadCleanup(service.clone());
    service.recover(vec![]).await.unwrap();
    service
        .query(
            principal(1),
            GitQuery::Inventory {
                workspace: WorkspaceId::new(server(), 1).unwrap(),
                cursor: None,
            },
        )
        .await
        .unwrap();
    let linked = fixture.root.join("linked");
    git(
        &fixture.main(),
        &["worktree", "add", "-b", "topic", linked.to_str().unwrap()],
    );
    {
        let mut current = actor.snapshot.lock().unwrap();
        let mut state = current.organization.state().clone();
        state.panes[0].starting_directory = directory(linked.as_os_str().as_encoded_bytes());
        current.organization = OrganizationSnapshot::new(
            current.organization.epoch(),
            state,
            current.organization.runtime().to_vec(),
        )
        .unwrap();
    }
    service.refresh_inventory(principal(1)).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let moved = {
                let snapshot = actor.snapshot.lock().unwrap();
                let state = snapshot.organization.state();
                state
                    .worktrees
                    .iter()
                    .any(|w| !w.main && state.tabs[0].worktree == Some(w.id))
            };
            if moved {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .unwrap();
    let published = actor.publications.lock().unwrap().clone();
    assert_eq!(
        published.last().unwrap().0,
        2,
        "every commit reaches the inactive client through the actor port"
    );
    service.shutdown().await.unwrap();
}

struct InventoryHost {
    fail: AtomicBool,
    records: Vec<ServerDirectory>,
}
impl ServerGitHost for InventoryHost {
    fn discover_source(
        &self,
        directory: &ServerDirectory,
        _: &GitAuthoritySnapshot,
        _: &GitReadCancellation,
    ) -> Result<Option<HostGitSource>, GitFailure> {
        Ok(self
            .records
            .iter()
            .find(|record| *record == directory)
            .map(|record| HostGitSource {
                common_directory: self::directory(b"/work/.git"),
                worktree: record.clone(),
            }))
    }
    fn inventory(
        &self,
        source: &ServerDirectory,
        basis: &GitAuthoritySnapshot,
        _: &GitReadCancellation,
    ) -> Result<Option<HostGitInventory>, GitFailure> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(GitFailure::Unchecked);
        }
        let repository = basis.repositories.first().map(|r| r.scope().repository);
        Ok(Some(HostGitInventory {
            source: source.clone(),
            current: self.records.iter().position(|record| record == source),
            lease: None,
            main_root: directory(b"/work"),
            common_directory: directory(b"/work/.git"),
            repository,
            records: self
                .records
                .iter()
                .enumerate()
                .map(|(index, directory)| HostGitWorktree {
                    retained: basis
                        .organization
                        .state()
                        .worktrees
                        .iter()
                        .find(|w| &w.directory == directory)
                        .map(|w| w.id),
                    directory: directory.clone(),
                    main: index == 0,
                    missing: false,
                    bare: false,
                    locked: false,
                    branch: None,
                })
                .collect(),
        }))
    }
    fn containment(
        &self,
        candidate: &ServerDirectory,
        root: &ServerDirectory,
    ) -> spaceterm_core::server_directories::DirectoryContainment {
        if candidate == root {
            spaceterm_core::server_directories::DirectoryContainment::Inside
        } else {
            spaceterm_core::server_directories::DirectoryContainment::Outside
        }
    }
    fn prepare_write(
        &self,
        _: HostGitWriteRequest,
        _: &GitReadCancellation,
    ) -> Result<Box<dyn PreparedServerGitWrite>, GitFailure> {
        Err(GitFailure::Unchecked)
    }
    fn reconcile(
        &self,
        _: &GitWriteIntent,
        _: &GitReadCancellation,
    ) -> Result<HostRecoveryObservation, GitFailure> {
        Err(GitFailure::Unchecked)
    }
    fn shutdown(&self) -> Result<(), GitFailure> {
        Ok(())
    }
}
#[tokio::test]
async fn inventory_pages_obey_byte_limits_and_failed_reads_do_not_erase_retained_records() {
    let actor = actor(snapshot());
    let mut records = vec![directory(b"/work")];
    records
        .extend((1..130).map(|i| directory(format!("/tree-{i}/{}", "x".repeat(3990)).as_bytes())));
    let host = Arc::new(InventoryHost {
        fail: AtomicBool::new(false),
        records,
    });
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        host.clone(),
        actor.clone(),
        GitLimits::default(),
    )
    .unwrap();
    let _cleanup = ReadCleanup(service.clone());
    service.recover(vec![]).await.unwrap();
    let workspace = WorkspaceId::new(server(), 1).unwrap();
    let mut response = service
        .query_request(
            principal(1),
            GitQueryRequest::new(
                GitCorrelation::new(1, 1).unwrap(),
                GitQuery::Inventory {
                    workspace,
                    cursor: None,
                },
                None,
                false,
            )
            .unwrap(),
        )
        .await;
    let mut count = 0;
    let mut original_cursor = None;
    let mut original_scope = None;
    loop {
        for major in [1, 2] {
            let payload =
                spaceterm_protocol::convert::git::encode_response(major, &response, server())
                    .unwrap();
            assert!(payload.encode().unwrap().len() < MAX_GIT_PAGE_BYTES);
            assert_eq!(
                spaceterm_protocol::convert::git::decode_response(&payload, server()).unwrap(),
                response
            );
        }
        let GitQueryResult::Inventory(page) = response.result.unwrap() else {
            panic!("inventory")
        };
        assert!(
            page.records.len() < 16,
            "directory bytes reduce the page below 128 records"
        );
        count += page.records.len();
        if let Some(cursor) = page.next {
            original_cursor.get_or_insert(cursor.clone());
            original_scope = response.scope;
            assert_eq!(
                service
                    .query(
                        principal(2),
                        GitQuery::Inventory {
                            workspace,
                            cursor: Some(cursor.clone())
                        }
                    )
                    .await,
                Err(GitFailure::Unauthorized)
            );
            response = service
                .query_request(
                    principal(1),
                    GitQueryRequest::new(
                        GitCorrelation::new(count as u64 + 1, 1).unwrap(),
                        GitQuery::Inventory {
                            workspace,
                            cursor: Some(cursor),
                        },
                        response.scope,
                        false,
                    )
                    .unwrap(),
                )
                .await;
        } else {
            break;
        }
    }
    assert_eq!(count, 130);
    let before = actor.snapshot.lock().unwrap().organization.clone();
    host.fail.store(true, Ordering::SeqCst);
    assert_eq!(
        service
            .query(
                principal(1),
                GitQuery::Inventory {
                    workspace,
                    cursor: None
                }
            )
            .await,
        Err(GitFailure::Unchecked)
    );
    assert_eq!(actor.snapshot.lock().unwrap().organization, before);
    {
        let mut current = actor.snapshot.lock().unwrap();
        let mut state = current.organization.state().clone();
        state.revision = state.revision.checked_successor().unwrap();
        current.organization = OrganizationSnapshot::new(
            current.organization.epoch(),
            state,
            current.organization.runtime().to_vec(),
        )
        .unwrap();
    }
    let response = service
        .query_request(
            principal(1),
            GitQueryRequest::new(
                GitCorrelation::new(500, 1).unwrap(),
                GitQuery::Inventory {
                    workspace,
                    cursor: original_cursor,
                },
                original_scope,
                false,
            )
            .unwrap(),
        )
        .await;
    assert_eq!(response.result, Err(GitFailure::StaleAssessment));
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_unavailable_unattached_workspace_does_not_block_other_inventory_reconciliation() {
    let fixture = NativeFixture::new("unavailable-unattached");
    let mut initial = native_actor(&fixture).snapshot.lock().unwrap().clone();
    let mut state = initial.organization.state().clone();
    let mut workspace = state.workspaces[0].clone();
    workspace.id = WorkspaceId::new(server(), 2).unwrap();
    workspace.root_tab = TabId::new(server(), 2).unwrap();
    workspace.creation_ordinal = CreationOrdinal::new(4);
    workspace.name.value.display = NameText::new("missing".into()).unwrap();
    let mut tab = state.tabs[0].clone();
    tab.id = workspace.root_tab;
    tab.workspace = workspace.id;
    tab.root_pane = PaneId::new(server(), 2).unwrap();
    tab.creation_ordinal = CreationOrdinal::new(5);
    let mut pane = state.panes[0].clone();
    pane.id = tab.root_pane;
    pane.tab = tab.id;
    pane.creation_ordinal = CreationOrdinal::new(6);
    pane.starting_directory =
        directory(fixture.root.join("missing").as_os_str().as_encoded_bytes());
    state.workspaces.push(workspace);
    state.tabs.push(tab);
    state.panes.push(pane);
    state.retained = None;
    let mut runtime = initial.organization.runtime().to_vec();
    runtime.push(PaneRuntimeRecord {
        pane: PaneId::new(server(), 2).unwrap(),
        ..runtime[0]
    });
    initial.organization = versioned_snapshot(initial.organization.epoch(), state, runtime);
    initial.runtime.panes.push(RuntimeWorkStamp {
        pane: PaneId::new(server(), 2).unwrap(),
        ..initial.runtime.panes[0].clone()
    });
    let actor = actor(initial);
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        fixture.host.clone(),
        actor.clone(),
        GitLimits::default(),
    )
    .unwrap();
    let _cleanup = ReadCleanup(service.clone());
    service.recover(vec![]).await.unwrap();
    let result = service
        .query(
            principal(1),
            GitQuery::Inventory {
                workspace: WorkspaceId::new(server(), 1).unwrap(),
                cursor: None,
            },
        )
        .await;
    assert!(result.is_ok());
    {
        let current = actor.snapshot.lock().unwrap();
        assert!(current.organization.state().tabs[0].worktree.is_some());
        assert!(
            current.organization.state().tabs[1].worktree.is_none(),
            "unavailable work retains its exact membership"
        );
    }
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn uncertain_inventory_commit_closes_admission_before_another_native_read() {
    let fixture = NativeFixture::new("uncertain-inventory");
    let actor = native_actor(&fixture);
    actor.uncertain.store(true, Ordering::SeqCst);
    let host = ControlledHost::new(fixture.host.clone());
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        host.clone(),
        actor.clone(),
        GitLimits::default(),
    )
    .unwrap();
    let _cleanup = ReadCleanup(service.clone());
    service.recover(vec![]).await.unwrap();
    assert_eq!(
        service
            .query(
                principal(1),
                GitQuery::Inventory {
                    workspace: WorkspaceId::new(server(), 1).unwrap(),
                    cursor: None
                }
            )
            .await,
        Err(GitFailure::IndeterminateDurability)
    );
    assert_eq!(
        service
            .query(
                principal(1),
                GitQuery::Status {
                    target: workspace_target()
                }
            )
            .await,
        Err(GitFailure::IndeterminateDurability)
    );
    assert_eq!(host.entered.load(Ordering::SeqCst), 0);
    assert!(actor.publications.lock().unwrap().is_empty());
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn reported_remote_directory_is_not_an_owning_server_probe_target() {
    use spaceterm_core::observations::*;
    let fixture = NativeFixture::new("remote-directory");
    let actor = native_actor(&fixture);
    {
        let mut current = actor.snapshot.lock().unwrap();
        let mut runtime = current.organization.runtime().to_vec();
        runtime[0].session_generation = SessionGeneration::new(1);
        runtime[0].phase = SessionPhase::Running {
            generation: SessionGeneration::new(1),
        };
        let session = SessionKey {
            epoch: current.organization.epoch(),
            pane: runtime[0].pane,
            generation: runtime[0].session_generation,
        };
        current.organization = versioned_snapshot(
            current.organization.epoch(),
            current.organization.state().clone(),
            runtime,
        );
        current.runtime.panes[0].session = Some(session);
        current.display_directories.push(
            DisplayDirectoryObservation::new(
                session,
                RuntimeObservationSequence::new(1).unwrap(),
                MonotonicTick::new(1),
                directory(b"/remote-only/path"),
                DirectoryPresentation::ReportedRemote,
            )
            .unwrap(),
        );
    }
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        fixture.host.clone(),
        actor,
        GitLimits::default(),
    )
    .unwrap();
    let _cleanup = ReadCleanup(service.clone());
    service.recover(vec![]).await.unwrap();
    let GitQueryResult::Status(status) = service
        .query(
            principal(1),
            GitQuery::Status {
                target: workspace_target(),
            },
        )
        .await
        .unwrap()
    else {
        panic!("status")
    };
    assert_eq!(status.branch.as_ref().unwrap().as_str(), "main");
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn concurrent_discovery_cannot_allocate_a_second_repository_from_an_old_observation() {
    let fixture = NativeFixture::new("concurrent-discovery");
    let actor = native_actor(&fixture);
    let host = ControlledHost::new(fixture.host.clone());
    host.block.store(true, Ordering::SeqCst);
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        host.clone(),
        actor.clone(),
        GitLimits::default(),
    )
    .unwrap();
    let _cleanup = ReadCleanup(service.clone());
    service.recover(vec![]).await.unwrap();
    let reader = service.clone();
    let delayed = tokio::spawn(async move {
        reader
            .query(
                principal(1),
                GitQuery::Status {
                    target: workspace_target(),
                },
            )
            .await
    });
    wait_count(&host.entered, 1).await;
    service
        .query(
            principal(2),
            GitQuery::Inventory {
                workspace: WorkspaceId::new(server(), 1).unwrap(),
                cursor: None,
            },
        )
        .await
        .unwrap();
    host.block.store(false, Ordering::SeqCst);
    assert_eq!(delayed.await.unwrap(), Err(GitFailure::StaleAssessment));
    assert_eq!(
        actor
            .snapshot
            .lock()
            .unwrap()
            .organization
            .state()
            .repositories
            .len(),
        1
    );
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn queued_status_is_fenced_at_egress_by_the_original_directory_binding() {
    let fixture = NativeFixture::new("status-egress-binding");
    let actor = native_actor(&fixture);
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        fixture.host.clone(),
        actor.clone(),
        GitLimits::default(),
    )
    .unwrap();
    let _cleanup = ReadCleanup(service.clone());
    service.recover(vec![]).await.unwrap();
    let mut subscription = service
        .subscribe_status(principal(1), workspace_target())
        .await
        .unwrap();
    let update = subscription.changed().await.unwrap();
    assert!(update.matches_snapshot(&actor.snapshot.lock().unwrap()));
    {
        let mut current = actor.snapshot.lock().unwrap();
        let mut state = current.organization.state().clone();
        state.panes[0].confirmed_restoration_directory = Versioned {
            value: Some(directory(fixture.root.as_os_str().as_encoded_bytes())),
            version: FieldVersion::new(2),
        };
        current.organization = OrganizationSnapshot::new(
            current.organization.epoch(),
            state,
            current.organization.runtime().to_vec(),
        )
        .unwrap();
    }
    assert!(!update.matches_snapshot(&actor.snapshot.lock().unwrap()));
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn removal_assessment_registers_a_new_linked_fallback_through_the_actor() {
    let fixture = NativeFixture::new("new-bare-fallback");
    let bare = fixture.root.join("bare.git");
    let target = fixture.root.join("target");
    let fallback = fixture.root.join("fallback");
    git(
        &fixture.main(),
        &[
            "clone",
            "--bare",
            fixture.main().to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
    );
    git(
        &bare,
        &["worktree", "add", "-b", "topic", target.to_str().unwrap()],
    );
    let actor = native_actor(&fixture);
    {
        let mut current = actor.snapshot.lock().unwrap();
        let mut state = current.organization.state().clone();
        state.panes[0].starting_directory = directory(target.as_os_str().as_encoded_bytes());
        current.organization = OrganizationSnapshot::new(
            current.organization.epoch(),
            state,
            current.organization.runtime().to_vec(),
        )
        .unwrap();
    }
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        fixture.host.clone(),
        actor.clone(),
        GitLimits::default(),
    )
    .unwrap();
    let _cleanup = ReadCleanup(service.clone());
    service.recover(vec![]).await.unwrap();
    service
        .query(
            principal(1),
            GitQuery::Inventory {
                workspace: WorkspaceId::new(server(), 1).unwrap(),
                cursor: None,
            },
        )
        .await
        .unwrap();
    let worktree = actor
        .snapshot
        .lock()
        .unwrap()
        .organization
        .state()
        .worktrees
        .iter()
        .find(|w| !w.main)
        .unwrap()
        .id;
    git(
        &bare,
        &[
            "worktree",
            "add",
            "-b",
            "surviving",
            fallback.to_str().unwrap(),
        ],
    );
    let GitQueryResult::Assessment(page) = service
        .query(principal(1), GitQuery::AssessRemove { worktree })
        .await
        .unwrap()
    else {
        panic!("assessment")
    };
    assert_eq!(page.fallbacks.len(), 1);
    assert_eq!(
        page.fallbacks[0].directory.bytes(),
        fallback.as_os_str().as_encoded_bytes()
    );
    assert!(
        actor
            .snapshot
            .lock()
            .unwrap()
            .organization
            .state()
            .worktrees
            .iter()
            .any(|w| Some(w.id) == page.fallbacks[0].worktree)
    );
    assert!(target.exists());
    assert_eq!(actor.unexpected_effects.load(Ordering::SeqCst), 0);
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn nested_linked_worktree_membership_uses_git_discovery_not_containment() {
    let fixture = NativeFixture::new("nested-linked-membership");
    let linked = fixture.main().join("linked");
    git(
        &fixture.main(),
        &["worktree", "add", "-b", "topic", linked.to_str().unwrap()],
    );
    let source = linked.join("source");
    std::fs::create_dir(&source).unwrap();
    let mut initial = native_actor(&fixture).snapshot.lock().unwrap().clone();
    let mut state = initial.organization.state().clone();
    state.panes[0].starting_directory = directory(source.as_os_str().as_encoded_bytes());
    let mut tab = state.tabs[0].clone();
    tab.id = TabId::new(server(), 2).unwrap();
    tab.root_pane = PaneId::new(server(), 2).unwrap();
    tab.creation_ordinal = CreationOrdinal::new(4);
    let mut pane = state.panes[0].clone();
    pane.id = tab.root_pane;
    pane.tab = tab.id;
    pane.creation_ordinal = CreationOrdinal::new(5);
    state.tabs.push(tab);
    state.panes.push(pane);
    state.retained = None;
    let mut runtime = initial.organization.runtime().to_vec();
    let mut second = runtime[0];
    second.pane = PaneId::new(server(), 2).unwrap();
    runtime.push(second);
    initial.organization = versioned_snapshot(initial.organization.epoch(), state, runtime);
    let actor = actor(initial);
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        fixture.host.clone(),
        actor.clone(),
        GitLimits::default(),
    )
    .unwrap();
    let _cleanup = ReadCleanup(service.clone());
    service.recover(vec![]).await.unwrap();
    service
        .query(
            principal(1),
            GitQuery::Inventory {
                workspace: WorkspaceId::new(server(), 1).unwrap(),
                cursor: None,
            },
        )
        .await
        .unwrap();
    {
        let current = actor.snapshot.lock().unwrap();
        let state = current.organization.state();
        let linked = state.worktrees.iter().find(|w| !w.main).unwrap().id;
        assert!(
            state.tabs.iter().all(|tab| tab.worktree == Some(linked)),
            "Root and unscoped Tabs follow Git's discovered linked Worktree"
        );
    }
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn nested_independent_repository_is_not_assigned_to_its_enclosing_repository() {
    let fixture = NativeFixture::new("nested-independent-membership");
    let nested = fixture.main().join("nested");
    std::fs::create_dir(&nested).unwrap();
    git(&nested, &["init", "-b", "nested"]);
    let mut initial = native_actor(&fixture).snapshot.lock().unwrap().clone();
    let mut state = initial.organization.state().clone();
    let mut workspace = state.workspaces[0].clone();
    workspace.id = WorkspaceId::new(server(), 2).unwrap();
    workspace.root_tab = TabId::new(server(), 2).unwrap();
    workspace.creation_ordinal = CreationOrdinal::new(4);
    workspace.name.value.display = NameText::new("nested".to_owned()).unwrap();
    let mut tab = state.tabs[0].clone();
    tab.id = workspace.root_tab;
    tab.workspace = workspace.id;
    tab.root_pane = PaneId::new(server(), 2).unwrap();
    tab.creation_ordinal = CreationOrdinal::new(5);
    let mut pane = state.panes[0].clone();
    pane.id = tab.root_pane;
    pane.tab = tab.id;
    pane.starting_directory = directory(nested.as_os_str().as_encoded_bytes());
    pane.creation_ordinal = CreationOrdinal::new(6);
    state.workspaces.push(workspace);
    state.tabs.push(tab);
    state.panes.push(pane);
    state.retained = None;
    let mut runtime = initial.organization.runtime().to_vec();
    let mut second = runtime[0];
    second.pane = PaneId::new(server(), 2).unwrap();
    runtime.push(second);
    initial.organization = versioned_snapshot(initial.organization.epoch(), state, runtime);
    let actor = actor(initial);
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        fixture.host.clone(),
        actor.clone(),
        GitLimits::default(),
    )
    .unwrap();
    let _cleanup = ReadCleanup(service.clone());
    service.recover(vec![]).await.unwrap();
    service
        .query(
            principal(1),
            GitQuery::Inventory {
                workspace: WorkspaceId::new(server(), 1).unwrap(),
                cursor: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        actor.snapshot.lock().unwrap().organization.state().tabs[1].worktree,
        None,
        "an enclosing inventory cannot assign a nested independent repository"
    );
    service
        .query(
            principal(1),
            GitQuery::Inventory {
                workspace: WorkspaceId::new(server(), 2).unwrap(),
                cursor: None,
            },
        )
        .await
        .unwrap();
    {
        let current = actor.snapshot.lock().unwrap();
        let state = current.organization.state();
        let roots = state
            .tabs
            .iter()
            .map(|t| {
                state
                    .worktrees
                    .iter()
                    .find(|w| Some(w.id) == t.worktree)
                    .unwrap()
                    .repository
            })
            .collect::<Vec<_>>();
        assert_ne!(roots[0], roots[1]);
    }
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn joining_a_dirty_follow_up_cannot_deliver_the_previous_source_generation() {
    let fixture = NativeFixture::new("joined-follow-up-source");
    let actor = native_actor(&fixture);
    let host = ControlledHost::new(fixture.host.clone());
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        host.clone(),
        actor.clone(),
        GitLimits::default(),
    )
    .unwrap();
    let _cleanup = ReadCleanup(service.clone());
    service.recover(vec![]).await.unwrap();
    let mut subscription = service
        .subscribe_status(principal(1), workspace_target())
        .await
        .unwrap();
    subscription.changed().await.unwrap().result.unwrap();
    host.block.store(true, Ordering::SeqCst);
    host.block_from.store(3, Ordering::SeqCst);
    service.refresh_statuses().unwrap();
    wait_count(&host.entered, 2).await;
    service.refresh_statuses().unwrap();
    host.block.store(false, Ordering::SeqCst);
    wait_count(&host.entered, 3).await;
    {
        let mut current = actor.snapshot.lock().unwrap();
        let mut state = current.organization.state().clone();
        state.panes[0].starting_directory = directory(fixture.root.as_os_str().as_encoded_bytes());
        current.organization = OrganizationSnapshot::new(
            current.organization.epoch(),
            state,
            current.organization.runtime().to_vec(),
        )
        .unwrap();
    }
    let querying = service.clone();
    let mut joined = tokio::spawn(async move {
        querying
            .query(
                principal(1),
                GitQuery::Status {
                    target: workspace_target(),
                },
            )
            .await
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut joined)
            .await
            .is_err(),
        "a fresh correlation must wait for the active generation, not consume repository A's retained result"
    );
    host.block_from.store(0, Ordering::SeqCst);
    assert_eq!(joined.await.unwrap(), Err(GitFailure::StaleAssessment));
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn native_status_keeps_last_known_facts_when_its_source_directory_disappears() {
    let fixture = NativeFixture::new("status-source-disappears");
    std::fs::write(fixture.main().join("change"), "test").unwrap();
    let actor = native_actor(&fixture);
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        fixture.host.clone(),
        actor.clone(),
        GitLimits::default(),
    )
    .unwrap();
    let _cleanup = ReadCleanup(service.clone());
    service.recover(vec![]).await.unwrap();
    let GitQueryResult::Status(first) = service
        .query(
            principal(1),
            GitQuery::Status {
                target: workspace_target(),
            },
        )
        .await
        .unwrap()
    else {
        panic!("status");
    };
    assert!(first.dirty && !first.unchecked);
    let committed = actor.snapshot.lock().unwrap().organization.clone();
    std::fs::rename(fixture.main(), fixture.root.join("renamed")).unwrap();
    let GitQueryResult::Status(last_known) = service
        .query(
            principal(1),
            GitQuery::Status {
                target: workspace_target(),
            },
        )
        .await
        .unwrap()
    else {
        panic!("status");
    };
    assert!(
        last_known.unchecked,
        "unavailable discovery is not a fresh empty status"
    );
    assert_eq!(last_known.branch, first.branch);
    assert_eq!(last_known.counts, first.counts);
    assert_eq!(last_known.changes, first.changes);
    assert!(last_known.dirty && last_known.revision > first.revision);
    assert_eq!(actor.snapshot.lock().unwrap().organization, committed);
    service.shutdown().await.unwrap();
}

struct RenameDuringDiscovery {
    armed: AtomicBool,
}
impl spaceterm_host::git::RepositoryProgramRunner for RenameDuringDiscovery {
    fn run(
        &self,
        request: &spaceterm_host::git::ProgramRequest,
        stdout: &mut dyn FnMut(&[u8]),
        cancellation: &GitReadCancellation,
    ) -> Result<spaceterm_host::git::ProgramExit, spaceterm_host::git::ProgramError> {
        let mut request = request.clone();
        if request.arguments.iter().any(|arg| arg == "--show-toplevel")
            && self.armed.swap(false, Ordering::SeqCst)
        {
            let renamed = request.directory.parent().unwrap().join("renamed");
            std::fs::rename(&request.directory, &renamed).unwrap();
            // A spawned child retains its cwd object when another process renames it,
            // while Git's subsequent absolute -C operand still names the vanished path.
            request.directory = renamed;
        }
        spaceterm_host::git::UnixRepositoryProgramRunner::new().run(&request, stdout, cancellation)
    }
}
#[tokio::test]
async fn failed_discovery_after_native_cwd_acquisition_retains_last_known_status() {
    let fixture = NativeFixture::new("status-source-discovery-race");
    std::fs::write(fixture.main().join("change"), "test").unwrap();
    let runner = Arc::new(RenameDuringDiscovery {
        armed: AtomicBool::new(false),
    });
    let version = spaceterm_host::git::tools::parse_git_version(
        &std::process::Command::new("/usr/bin/git")
            .arg("--version")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    let host = Arc::new(NativeServerGitHost::new(
        runner.clone(),
        "/usr/bin/git".into(),
        fixture.root.clone(),
        version,
        Arc::new(
            spaceterm_host::server_directories::native::NativeDirectoryProvider::new(
                server(),
                fixture.root.clone(),
            )
            .unwrap(),
        ),
    ));
    let actor = native_actor(&fixture);
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        host,
        actor.clone(),
        GitLimits::default(),
    )
    .unwrap();
    let _cleanup = ReadCleanup(service.clone());
    service.recover(vec![]).await.unwrap();
    let GitQueryResult::Status(first) = service
        .query(
            principal(1),
            GitQuery::Status {
                target: workspace_target(),
            },
        )
        .await
        .unwrap()
    else {
        panic!("status");
    };
    let committed = actor.snapshot.lock().unwrap().organization.clone();
    runner.armed.store(true, Ordering::SeqCst);
    let GitQueryResult::Status(last_known) = service
        .query(
            principal(1),
            GitQuery::Status {
                target: workspace_target(),
            },
        )
        .await
        .unwrap()
    else {
        panic!("status");
    };
    assert!(
        last_known.unchecked && last_known.dirty,
        "a failed -C discovery must not become a checked empty observation"
    );
    assert_eq!(last_known.counts, first.counts);
    assert_eq!(last_known.branch, first.branch);
    assert_eq!(actor.snapshot.lock().unwrap().organization, committed);
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn checked_outside_source_unassigns_the_root_even_when_old_git_metadata_is_unavailable() {
    for route in ["inventory", "status", "maintenance"] {
        let fixture = NativeFixture::new(&format!("outside-root-{route}"));
        let actor = native_actor(&fixture);
        let service = GitService::new(
            server(),
            ServerEpoch::new(1),
            fixture.host.clone(),
            actor.clone(),
            GitLimits::default(),
        )
        .unwrap();
        let _cleanup = ReadCleanup(service.clone());
        service.recover(vec![]).await.unwrap();
        let workspace = WorkspaceId::new(server(), 1).unwrap();
        service
            .query(
                principal(1),
                GitQuery::Inventory {
                    workspace,
                    cursor: None,
                },
            )
            .await
            .unwrap();
        let before = actor.snapshot.lock().unwrap().organization.state().clone();
        assert!(before.tabs[0].worktree.is_some());
        std::fs::rename(
            fixture.main().join(".git"),
            fixture.root.join("unavailable-git"),
        )
        .unwrap();
        let outside = fixture.root.join("outside");
        std::fs::create_dir(&outside).unwrap();
        {
            let mut current = actor.snapshot.lock().unwrap();
            let mut state = current.organization.state().clone();
            state.panes[0].confirmed_restoration_directory.value =
                Some(directory(outside.as_os_str().as_encoded_bytes()));
            state.panes[0].confirmed_restoration_directory.version = FieldVersion::new(2);
            state.revision = state.revision.checked_successor().unwrap();
            current.organization = OrganizationSnapshot::new(
                current.organization.epoch(),
                state,
                current.organization.runtime().to_vec(),
            )
            .unwrap();
        }
        let publications = actor.publications.lock().unwrap().len();
        match route {
            "inventory" => {
                let GitQueryResult::Inventory(page) = service
                    .query(
                        principal(1),
                        GitQuery::Inventory {
                            workspace,
                            cursor: None,
                        },
                    )
                    .await
                    .unwrap()
                else {
                    panic!("inventory");
                };
                assert!(page.records.is_empty());
            }
            "status" => {
                let GitQueryResult::Status(facts) = service
                    .query(
                        principal(1),
                        GitQuery::Status {
                            target: workspace_target(),
                        },
                    )
                    .await
                    .unwrap()
                else {
                    panic!("status");
                };
                assert!(facts.branch.is_none() && !facts.unchecked);
            }
            _ => {
                service.refresh_inventory(principal(1)).unwrap();
                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    while actor.snapshot.lock().unwrap().organization.state().tabs[0]
                        .worktree
                        .is_some()
                    {
                        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                    }
                })
                .await
                .unwrap();
            }
        }
        {
            let current = actor.snapshot.lock().unwrap();
            let state = current.organization.state();
            assert_eq!(
                state.tabs[0].worktree, None,
                "the checked outside source follows the Root directory without relying on the old repository being readable"
            );
            assert_eq!(
                state.worktrees, before.worktrees,
                "outside reconciliation never deletes retained inventory"
            );
            assert_eq!(state.repositories, before.repositories);
            assert!(
                actor.publications.lock().unwrap().len() > publications,
                "the committed membership change reaches every authorized client"
            );
        }
        service.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn quota_deferred_status_subscriptions_refresh_fairly_without_manual_queries() {
    let fixture = NativeFixture::new("subscription-fairness");
    let actor = actor(with_panes(
        native_actor(&fixture).snapshot.lock().unwrap().clone(),
        5,
    ));
    let host = ControlledHost::new(fixture.host.clone());
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        host.clone(),
        actor,
        GitLimits::default(),
    )
    .unwrap();
    let _cleanup = ReadCleanup(service.clone());
    service.recover(vec![]).await.unwrap();
    let mut subscriptions = Vec::new();
    for index in 1..=5 {
        let mut subscription = service
            .subscribe_status(
                principal(1),
                GitStatusTarget::Pane(PaneId::new(server(), index).unwrap()),
            )
            .await
            .unwrap();
        let first = subscription.changed().await.unwrap().result.unwrap();
        subscriptions.push((subscription, first.revision));
    }
    let entered = host.entered.load(Ordering::SeqCst);
    host.block.store(true, Ordering::SeqCst);
    service.refresh_statuses().unwrap();
    wait_count(&host.entered, entered + 4).await;
    for _ in 0..20 {
        service.refresh_statuses().unwrap();
    }
    host.block.store(false, Ordering::SeqCst);
    for (subscription, revision) in &mut subscriptions {
        tokio::time::timeout(std::time::Duration::from_secs(6), async {
            loop {
                if subscription
                    .changed()
                    .await
                    .unwrap()
                    .result
                    .is_ok_and(|facts| facts.revision > *revision)
                {
                    break;
                }
            }
        })
        .await
        .expect("every accepted target must advance after capacity returns");
    }
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn rejected_native_inventory_releases_its_leases_before_another_repository_read() {
    let fixture = NativeFixture::new("inventory-lease-rollback");
    for index in 1..32 {
        let linked = fixture.root.join(format!("linked-{index}"));
        git(
            &fixture.main(),
            &[
                "worktree",
                "add",
                "-b",
                &format!("topic-{index}"),
                linked.to_str().unwrap(),
            ],
        );
    }
    let actor = native_actor(&fixture);
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        fixture.host.clone(),
        actor.clone(),
        GitLimits::default(),
    )
    .unwrap();
    let _cleanup = ReadCleanup(service.clone());
    service.recover(vec![]).await.unwrap();
    let workspace = WorkspaceId::new(server(), 1).unwrap();
    assert_eq!(
        service
            .query(
                principal(1),
                GitQuery::Inventory {
                    workspace,
                    cursor: None
                }
            )
            .await,
        Err(GitFailure::Capacity)
    );
    assert!(
        actor
            .snapshot
            .lock()
            .unwrap()
            .organization
            .state()
            .repositories
            .is_empty()
    );
    let small = fixture.root.join("small");
    std::fs::create_dir(&small).unwrap();
    git(&small, &["init", "-b", "small"]);
    {
        let mut current = actor.snapshot.lock().unwrap();
        let mut state = current.organization.state().clone();
        state.panes[0].starting_directory = directory(small.as_os_str().as_encoded_bytes());
        current.organization = OrganizationSnapshot::new(
            current.organization.epoch(),
            state,
            current.organization.runtime().to_vec(),
        )
        .unwrap();
    }
    let GitQueryResult::Inventory(page) = service
        .query(
            principal(1),
            GitQuery::Inventory {
                workspace,
                cursor: None,
            },
        )
        .await
        .expect("a rejected native observation cannot exhaust later discovery")
    else {
        panic!("inventory");
    };
    assert_eq!(page.records.len(), 1);
    assert_eq!(
        page.records[0].directory,
        directory(small.as_os_str().as_encoded_bytes())
    );
    let committed = actor.snapshot.lock().unwrap().organization.clone();
    std::fs::rename(small.join(".git"), fixture.root.join("old-small-git")).unwrap();
    git(&small, &["init", "-b", "replacement"]);
    assert_eq!(
        service
            .query(
                principal(1),
                GitQuery::Inventory {
                    workspace,
                    cursor: None
                }
            )
            .await,
        Err(GitFailure::IdentityReplaced),
        "rollback of an uncommitted listing must not discard committed identity fences"
    );
    assert_eq!(actor.snapshot.lock().unwrap().organization, committed);
    service.shutdown().await.unwrap();
}

struct BudgetHost {
    branches: Vec<GitBranchRecord>,
}
impl ServerGitHost for BudgetHost {
    fn status(
        &self,
        _: &ServerDirectory,
        _: &GitAuthoritySnapshot,
        _: &GitReadCancellation,
    ) -> Result<HostGitStatus, GitFailure> {
        Ok(HostGitStatus {
            inventory: None,
            facts: GitStatusFacts {
                revision: std::num::NonZeroU64::new(1).unwrap(),
                branch: None,
                changes: vec![],
                dirty: false,
                unchecked: false,
                pull_request: None,
                upstream: None,
                operation: None,
                counts: None,
            },
        })
    }
    fn branches(
        &self,
        _: &RepositoryLocator,
        _: &GitReadCancellation,
    ) -> Result<Vec<GitBranchRecord>, GitFailure> {
        Ok(self.branches.clone())
    }
    fn prepare_write(
        &self,
        _: HostGitWriteRequest,
        _: &GitReadCancellation,
    ) -> Result<Box<dyn PreparedServerGitWrite>, GitFailure> {
        Err(GitFailure::Unchecked)
    }
    fn reconcile(
        &self,
        _: &GitWriteIntent,
        _: &GitReadCancellation,
    ) -> Result<HostRecoveryObservation, GitFailure> {
        Err(GitFailure::Unchecked)
    }
    fn shutdown(&self) -> Result<(), GitFailure> {
        Ok(())
    }
}
struct BudgetProof;
impl GitHostAssessment for BudgetProof {
    fn prepare(
        &self,
        _: &GitWriteIntent,
        _: &GitReadCancellation,
    ) -> Result<Box<dyn PreparedServerGitWrite>, GitFailure> {
        Err(GitFailure::Unchecked)
    }
}
fn budget_assessment(panes: u64) -> (GitAuthoritySnapshot, ConfirmedGitPlan) {
    let mut initial = with_panes(snapshot(), panes);
    let mut state = initial.organization.state().clone();
    let repository = RepositoryId::new(server(), 1).unwrap();
    let main = WorktreeRecord {
        id: WorktreeId::new(server(), 1).unwrap(),
        repository,
        directory: directory(b"/repository"),
        main: true,
        missing: false,
    };
    let linked = WorktreeRecord {
        id: WorktreeId::new(server(), 2).unwrap(),
        repository,
        directory: directory(b"/linked"),
        main: false,
        missing: false,
    };
    state.repositories.push(RepositoryRecord {
        id: repository,
        directory: main.directory.clone(),
    });
    state.worktrees.extend([main.clone(), linked.clone()]);
    state.tabs[0].worktree = Some(linked.id);
    for pane in &mut state.panes {
        pane.starting_directory = directory(format!("/{}", "x".repeat(4090)).as_bytes());
    }
    state.retained = None;
    initial.organization = versioned_snapshot(
        initial.organization.epoch(),
        state,
        initial.organization.runtime().to_vec(),
    );
    let locator = RepositoryLocator::new(RepositoryLocatorParts {
        scope: RepositoryScope {
            server: server(),
            repository,
        },
        main_root: main.directory.clone(),
        common_directory: directory(b"/repository/.git"),
    })
    .unwrap();
    initial.repositories.push(locator.clone());
    let scope = assess_removal(
        &initial.organization,
        &initial.runtime,
        &WorktreeRemovalFacts {
            target: linked,
            listed: true,
            locked: false,
            head: None,
            changes: false,
            submodules: false,
        },
        &AffectedWorktreeWork {
            pinned: vec![],
            panes: initial
                .organization
                .state()
                .panes
                .iter()
                .map(|pane| PaneDirectoryContainment {
                    pane: pane.id,
                    directory: pane.restoration_directory().clone(),
                    containment: spaceterm_core::server_directories::DirectoryContainment::Outside,
                })
                .collect(),
            fallbacks: vec![FallbackWorktree {
                record: main,
                bare: false,
                available: true,
                containment: spaceterm_core::server_directories::DirectoryContainment::Outside,
            }],
        },
    )
    .unwrap();
    let plan = ConfirmedGitPlan {
        repository: locator,
        assessment_digest: scope.assessment_digest(),
        action: ConfirmedGitAction::Remove(Box::new(scope)),
    };
    (initial, plan)
}
#[tokio::test]
async fn one_page_assessments_and_paged_reads_share_one_residency_ceiling() {
    let (initial, plan) = budget_assessment(120);
    let ConfirmedGitAction::Remove(scope) = &plan.action else {
        unreachable!();
    };
    let assessment_bytes = 2048
        + plan.repository.parts().main_root.bytes().len()
        + plan.repository.parts().common_directory.bytes().len()
        + scope.value_bytes();
    let branches = (0..1024)
        .map(|index| GitBranchRecord {
            name: BranchName::new(format!("b{index:04}/{}", "x".repeat(994))).unwrap(),
            remote: false,
            checked_out: false,
        })
        .collect::<Vec<_>>();
    let branch_bytes = 512
        + 128
        + branches
            .iter()
            .map(|r| 128 + r.name.as_str().len())
            .sum::<usize>();
    let expected_pages = (16 * 1024 * 1024 - assessment_bytes * 24) / branch_bytes;
    assert!((1..8).contains(&expected_pages));
    let repository = plan.repository.scope();
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        Arc::new(BudgetHost { branches }),
        actor(initial),
        GitLimits::default(),
    )
    .unwrap();
    let _cleanup = ReadCleanup(service.clone());
    service.recover(vec![]).await.unwrap();
    for index in 0..24 {
        let owner = principal(index / 8 + 1);
        let token = service
            .cache_assessment(owner, plan.clone(), Arc::new(BudgetProof))
            .await
            .unwrap();
        let GitQueryResult::Assessment(page) = service
            .query(
                owner,
                GitQuery::AssessmentPage {
                    assessment: token,
                    cursor: None,
                },
            )
            .await
            .unwrap()
        else {
            panic!("assessment");
        };
        assert!(
            page.next.is_none(),
            "these plans have no page observation to account for them indirectly"
        );
    }
    for index in 0..expected_pages {
        let GitQueryResult::Branches(page) = service
            .query(
                principal(index as u64 + 1),
                GitQuery::Branches {
                    repository,
                    cursor: None,
                },
            )
            .await
            .unwrap()
        else {
            panic!("branches");
        };
        assert!(page.next.is_some());
    }
    assert_eq!(
        service
            .query(
                principal(expected_pages as u64 + 1),
                GitQuery::Branches {
                    repository,
                    cursor: None
                }
            )
            .await,
        Err(GitFailure::Capacity),
        "aggregate retained plans and pages must reject the first observation crossing 16 MiB"
    );
    service.shutdown().await.unwrap();
}

async fn full_assessment_cache() -> (GitService, RepositoryScope, [GitAssessmentToken; 2]) {
    let (mut initial, plan) = budget_assessment(1873);
    let mut state = initial.organization.state().clone();
    state.tabs[0].worktree = None;
    initial.organization = versioned_snapshot(
        initial.organization.epoch(),
        state,
        initial.organization.runtime().to_vec(),
    );
    let ConfirmedGitAction::Remove(scope) = &plan.action else {
        unreachable!()
    };
    let bytes = 2048
        + plan.repository.parts().main_root.bytes().len()
        + plan.repository.parts().common_directory.bytes().len()
        + scope.value_bytes();
    assert!(bytes * 2 < 16 * 1024 * 1024);
    assert!(16 * 1024 * 1024 - bytes * 2 < 8192);
    let repository = plan.repository.scope();
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        Arc::new(BudgetHost {
            branches: (0..256)
                .map(|index| GitBranchRecord {
                    name: BranchName::new(format!("b{index:04}/{}", "x".repeat(994))).unwrap(),
                    remote: false,
                    checked_out: false,
                })
                .collect(),
        }),
        actor(initial),
        GitLimits::default(),
    )
    .unwrap();
    service.recover(vec![]).await.unwrap();
    let token = service
        .cache_assessment(principal(1), plan.clone(), Arc::new(BudgetProof))
        .await
        .unwrap();
    let other = service
        .cache_assessment(principal(1), plan, Arc::new(BudgetProof))
        .await
        .unwrap();
    (service, repository, [token, other])
}

#[tokio::test(start_paused = true)]
async fn expired_assessments_release_residency_before_status_admission() {
    let (service, _, [token, _]) = full_assessment_cache().await;
    let _cleanup = ReadCleanup(service.clone());
    let query = GitQuery::Status {
        target: GitStatusTarget::Pane(PaneId::new(server(), 1).unwrap()),
    };
    assert_eq!(
        service.query(principal(1), query.clone()).await,
        Err(GitFailure::Capacity)
    );
    tokio::time::advance(std::time::Duration::from_secs(121)).await;
    let result = service.query(principal(1), query).await;
    let expired = service
        .query(
            principal(1),
            GitQuery::AssessmentPage {
                assessment: token,
                cursor: None,
            },
        )
        .await;
    service.shutdown().await.unwrap();
    assert!(
        matches!(result, Ok(GitQueryResult::Status(_))),
        "expired unused plans cannot starve status reads: {result:?}"
    );
    assert_eq!(expired, Err(GitFailure::ExpiredAssessment));
}

#[tokio::test(start_paused = true)]
async fn expired_assessments_release_residency_before_branch_page_admission() {
    let (service, repository, tokens) = full_assessment_cache().await;
    let _cleanup = ReadCleanup(service.clone());
    let query = GitQuery::Branches {
        repository,
        cursor: None,
    };
    assert_eq!(
        service.query(principal(1), query.clone()).await,
        Err(GitFailure::Capacity)
    );
    tokio::time::advance(std::time::Duration::from_secs(119)).await;
    for token in &tokens {
        let GitQueryResult::Assessment(page) = service
            .query(
                principal(1),
                GitQuery::AssessmentPage {
                    assessment: token.clone(),
                    cursor: None,
                },
            )
            .await
            .unwrap()
        else {
            panic!("assessment")
        };
        assert!(page.next.is_some());
    }
    // These pages are only two seconds old when their assessment tokens expire.
    tokio::time::advance(std::time::Duration::from_secs(2)).await;
    let result = service.query(principal(1), query).await;
    let expired = service
        .query(
            principal(1),
            GitQuery::AssessmentPage {
                assessment: tokens[0].clone(),
                cursor: None,
            },
        )
        .await;
    service.shutdown().await.unwrap();
    assert!(
        matches!(result, Ok(GitQueryResult::Branches(ref page)) if page.next.is_some()),
        "expired unused plans cannot starve branch pages: {result:?}"
    );
    assert_eq!(expired, Err(GitFailure::ExpiredAssessment));
}

#[tokio::test]
async fn assessment_pages_share_their_plan_reservation_with_the_cached_tokens() {
    let (initial, plan) = budget_assessment(1800);
    let service = GitService::new(
        server(),
        ServerEpoch::new(1),
        Arc::new(BudgetHost { branches: vec![] }),
        actor(initial),
        GitLimits::default(),
    )
    .unwrap();
    let _cleanup = ReadCleanup(service.clone());
    service.recover(vec![]).await.unwrap();
    for _ in 0..2 {
        let token = service
            .cache_assessment(principal(1), plan.clone(), Arc::new(BudgetProof))
            .await
            .unwrap();
        let GitQueryResult::Assessment(page) = service
            .query(
                principal(1),
                GitQuery::AssessmentPage {
                    assessment: token.clone(),
                    cursor: None,
                },
            )
            .await
            .expect("two distinct plans fit; pages must not charge each shared plan again")
        else {
            panic!("assessment");
        };
        let cursor = page.next.unwrap();
        service
            .query(
                principal(1),
                GitQuery::AssessmentPage {
                    assessment: token,
                    cursor: Some(cursor),
                },
            )
            .await
            .unwrap();
    }
    assert_eq!(
        service
            .cache_assessment(principal(1), plan, Arc::new(BudgetProof))
            .await,
        Err(GitFailure::Capacity),
        "a third distinct plan exceeds the shared residency ceiling"
    );
    service.shutdown().await.unwrap();
}

#[test]
fn checked_native_discovery_preserves_intentional_status_suppression() {
    let fixture = NativeFixture::new("checked-discovery-suppression");
    let bare = fixture.root.join("bare");
    std::fs::create_dir(&bare).unwrap();
    git(&bare, &["init", "--bare"]);
    let home_child = fixture.root.join("shell");
    std::fs::create_dir(&home_child).unwrap();
    git(&fixture.root, &["init", "-b", "home"]);
    for selected in [fixture.main().join(".git"), bare, home_child] {
        let selected = directory(selected.as_os_str().as_encoded_bytes());
        let observed = fixture
            .host
            .status(&selected, &snapshot(), &GitReadCancellation::default())
            .expect("checked Git presentation suppression is not a failed native probe");
        assert!(observed.inventory.is_none());
        assert!(observed.facts.branch.is_none() && !observed.facts.unchecked);
        assert!(
            fixture
                .host
                .inventory(&selected, &snapshot(), &GitReadCancellation::default())
                .unwrap()
                .is_none()
        );
    }
}
