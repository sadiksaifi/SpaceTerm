//! Owning-server reads. Cached observations and continuations bind the authenticated principal.
use super::authority::*;
use super::host::*;
use super::writes::GitService;
use spaceterm_core::authority::{FieldVersion, OrganizationRevision, SessionGeneration};
use spaceterm_core::identity::{PaneId, RepositoryId, WorkspaceId};
use spaceterm_core::observations::{DirectoryPresentation, RuntimeObservationSequence};
use spaceterm_core::operations::RepositoryScope;
use spaceterm_core::organization::ServerDirectory;
use spaceterm_core::worktrees::{queries::*, *};
use std::collections::{BTreeMap, VecDeque};
use std::num::NonZeroU64;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;

const MAX_READ_JOBS: usize = 32;
const MAX_CLIENT_READS: usize = 4;
const MAX_READ_BYTES: usize = 16 * 1024 * 1024;
const CACHE_LIFETIME: Duration = Duration::from_secs(120);
const READ_DEADLINE: Duration = Duration::from_secs(30);
const REFRESH_INTERVAL: Duration = Duration::from_secs(2);

type ScopedResult = Result<(Option<RepositoryScope>, GitQueryResult), GitFailure>;
#[derive(Clone)]
struct ReadResult {
    result: ScopedResult,
    source: Option<(GitStatusTarget, SourceBinding)>,
}

pub(crate) struct CachedAssessment {
    pub token: GitAssessmentToken,
    pub principal: AuthenticatedGitPrincipal,
    pub plan: Arc<ConfirmedGitPlan>,
    pub native: Arc<dyn GitHostAssessment>,
    pub created: Instant,
    pub residency: Arc<ReadReservation>,
}

pub(crate) struct ReadReservation {
    pool: Arc<AtomicUsize>,
    bytes: usize,
}
impl ReadReservation {
    fn reserve(pool: &Arc<AtomicUsize>, bytes: usize) -> Result<Self, GitFailure> {
        pool.fetch_update(Ordering::AcqRel, Ordering::Relaxed, |used| {
            used.checked_add(bytes)
                .filter(|total| *total <= MAX_READ_BYTES)
        })
        .map_err(|_| GitFailure::Capacity)?;
        Ok(Self {
            pool: pool.clone(),
            bytes,
        })
    }
    fn grow(&mut self, bytes: usize) -> Result<(), GitFailure> {
        self.pool
            .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |used| {
                used.checked_add(bytes)
                    .filter(|total| *total <= MAX_READ_BYTES)
            })
            .map_err(|_| GitFailure::Capacity)?;
        self.bytes += bytes;
        Ok(())
    }
}
impl Drop for ReadReservation {
    fn drop(&mut self) {
        self.pool.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

#[derive(Default)]
pub(crate) struct ReadState {
    residency: Arc<AtomicUsize>,
    jobs: Vec<ReadJob>,
    statuses: Vec<CachedStatus>,
    pages: Vec<PageObservation>,
    interests: Vec<StatusInterest>,
    pending_statuses: VecDeque<(AuthenticatedGitPrincipal, GitStatusTarget)>,
    poller: Option<JoinHandle<()>>,
    maintenance: Option<InventoryMaintenance>,
    inventory_offset: usize,
    principals: Vec<AuthenticatedGitPrincipal>,
    revision: u64,
    observation: u64,
}
struct InventoryMaintenance {
    handle: JoinHandle<()>,
    pending: Option<AuthenticatedGitPrincipal>,
    pending_reserved: bool,
    active: Option<AuthenticatedGitPrincipal>,
}
impl InventoryMaintenance {
    fn owner(&self) -> Option<AuthenticatedGitPrincipal> {
        self.active
            .or(self.pending.filter(|_| self.pending_reserved))
    }
    fn attributed_to(&self, principal: AuthenticatedGitPrincipal) -> bool {
        self.active == Some(principal) || (self.pending_reserved && self.pending == Some(principal))
    }
}
struct ReadJob {
    principal: AuthenticatedGitPrincipal,
    query: GitQuery,
    receiver: watch::Receiver<Option<ReadResult>>,
    handle: JoinHandle<()>,
    active: bool,
    dirty: bool,
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceBinding {
    directory: ServerDirectory,
    pane: Option<PaneId>,
    session: SessionGeneration,
    sequence: Option<RuntimeObservationSequence>,
    version: FieldVersion,
}
struct CachedStatus {
    principal: AuthenticatedGitPrincipal,
    target: GitStatusTarget,
    source: SourceBinding,
    scope: Option<RepositoryScope>,
    facts: GitStatusFacts,
    updated: Instant,
    _residency: ReadReservation,
}
struct StatusInterest {
    principal: AuthenticatedGitPrincipal,
    target: GitStatusTarget,
    sender: watch::Sender<Option<GitStatusUpdate>>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitStatusUpdate {
    pub epoch: spaceterm_core::authority::ServerEpoch,
    pub target: GitStatusTarget,
    pub scope: Option<RepositoryScope>,
    pub result: Result<GitStatusFacts, GitFailure>,
    source: Option<SourceBinding>,
}
impl GitStatusUpdate {
    /// The actor's egress boundary checks this immediately before encoding.
    /// Directory and Session changes cannot make a queued status current again.
    pub fn matches_snapshot(&self, snapshot: &GitAuthoritySnapshot) -> bool {
        self.epoch == snapshot.organization.epoch()
            && self.target.server() == snapshot.organization.state().server
            && self
                .source
                .as_ref()
                .is_none_or(|source| resolve_source(snapshot, self.target).as_ref() == Ok(source))
    }
}
/// One replaceable latest status, not an unbounded delivery queue. Dropping a
/// subscription never cancels authoritative inventory work or a Started write.
pub struct GitStatusSubscription {
    receiver: watch::Receiver<Option<GitStatusUpdate>>,
    delivered: bool,
}
impl GitStatusSubscription {
    pub async fn changed(&mut self) -> Result<GitStatusUpdate, GitFailure> {
        if self.delivered {
            self.receiver
                .changed()
                .await
                .map_err(|_| GitFailure::CancelledRead)?;
        }
        loop {
            if let Some(update) = self.receiver.borrow_and_update().clone() {
                self.delivered = true;
                return Ok(update);
            }
            self.receiver
                .changed()
                .await
                .map_err(|_| GitFailure::CancelledRead)?;
        }
    }
    pub fn latest(&self) -> Option<GitStatusUpdate> {
        self.receiver.borrow().clone()
    }
}

#[derive(Clone)]
enum PageData {
    Inventory {
        workspace: WorkspaceId,
        source: SourceBinding,
        revision: OrganizationRevision,
        records: Vec<GitInventoryRecord>,
    },
    Branches {
        records: Vec<GitBranchRecord>,
        version: Option<FieldVersion>,
    },
    Assessment {
        token: GitAssessmentToken,
        plan: Arc<ConfirmedGitPlan>,
        _residency: Arc<ReadReservation>,
    },
}
struct PageObservation {
    principal: AuthenticatedGitPrincipal,
    scope: RepositoryScope,
    observation: u64,
    data: PageData,
    cursors: BTreeMap<[u8; 32], usize>,
    residency: Option<ReadReservation>,
    created: Instant,
}
impl PageObservation {
    fn bytes(&self) -> usize {
        512 + self.cursors.len() * 128
            + match &self.data {
                PageData::Inventory {
                    source, records, ..
                } => {
                    source.directory.bytes().len()
                        + records
                            .iter()
                            .map(|r| 128 + r.directory.bytes().len())
                            .sum::<usize>()
                }
                PageData::Branches { records, .. } => {
                    records.iter().map(|r| 128 + r.name.as_str().len()).sum()
                }
                // Token, native proof and pages share one reservation for the immutable plan.
                PageData::Assessment { .. } => 0,
            }
    }
}

impl GitService {
    /// Expiration drops cache ownership, never a reservation held by active native work.
    fn expire_read_cache(&self) -> Result<(), GitFailure> {
        let now = Instant::now();
        // Keep state before reads so a concurrent insertion cannot lose its new page.
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| GitFailure::StorageFailure)?;
        state
            .assessments
            .retain(|_, entry| now.duration_since(entry.created) < CACHE_LIFETIME);
        let mut reads = self
            .inner
            .reads
            .lock()
            .map_err(|_| GitFailure::StorageFailure)?;
        reads
            .statuses
            .retain(|entry| now.duration_since(entry.updated) < CACHE_LIFETIME);
        reads.pages.retain(|page| {
            now.duration_since(page.created) < CACHE_LIFETIME
                && match &page.data {
                    PageData::Assessment { token, .. } => {
                        state.assessments.contains_key(&token.parts().random)
                    }
                    _ => true,
                }
        });
        Ok(())
    }

    pub async fn query(
        &self,
        principal: AuthenticatedGitPrincipal,
        query: GitQuery,
    ) -> Result<GitQueryResult, GitFailure> {
        self.query_scoped(principal, query)
            .await
            .map(|(_, result)| result)
    }

    /// Both-major adapters preserve correlation and may assert an already known
    /// scope. That assertion never supplies discovery or local filesystem authority.
    /// For subscribed requests, the connection adapter retains a `subscribe_status`
    /// receiver and encodes its updates using this request's correlation.
    pub async fn query_request(
        &self,
        principal: AuthenticatedGitPrincipal,
        request: GitQueryRequest,
    ) -> GitQueryResponse {
        let correlation = request.correlation;
        let result = match request.validate() {
            Ok(()) => self.query_scoped(principal, request.query).await,
            Err(error) => Err(error),
        };
        match result {
            Ok((scope, result)) if request.scope.is_none() || request.scope == scope => {
                GitQueryResponse {
                    correlation,
                    scope,
                    result: Ok(result),
                }
            }
            Ok(_) => GitQueryResponse {
                correlation,
                scope: None,
                result: Err(GitFailure::StaleAssessment),
            },
            Err(error) => GitQueryResponse {
                correlation,
                scope: None,
                result: Err(error),
            },
        }
    }

    async fn query_scoped(
        &self,
        principal: AuthenticatedGitPrincipal,
        query: GitQuery,
    ) -> ScopedResult {
        self.check_principal(principal)?;
        query.validate_scope(self.inner.server)?;
        self.admission()?;
        self.inner.organization.authorize(principal).await?;
        self.start_read_poller(principal)?;
        if let Some(cursor) = query_cursor(&query) {
            return self.continue_page(principal, &query, cursor).await;
        }
        if let GitQuery::AssessmentPage { assessment, .. } = &query {
            let (plan, residency) = self.assessment_plan(principal, assessment)?;
            return self.first_page(
                principal,
                plan.repository.scope(),
                PageData::Assessment {
                    token: assessment.clone(),
                    plan,
                    _residency: residency,
                },
            );
        }
        let mut receiver = self.schedule_read(principal, query, false)?;
        loop {
            let result = receiver.borrow().clone();
            if let Some(result) = result {
                if self.inner.cancellation.is_cancelled() {
                    return Err(GitFailure::CancelledRead);
                }
                // Revocation and shutdown fence delivery even when a caller joined an older job.
                self.admission()?;
                let deadline = tokio::time::Instant::now() + READ_DEADLINE;
                self.read_wait(deadline, self.inner.organization.authorize(principal))
                    .await??;
                if let Some((target, source)) = &result.source {
                    let current = self
                        .read_wait(deadline, self.inner.organization.snapshot())
                        .await??;
                    self.validate_snapshot(&current)?;
                    if resolve_source(&current, *target).as_ref() != Ok(source) {
                        return Err(GitFailure::StaleAssessment);
                    }
                }
                return result.result;
            }
            receiver
                .changed()
                .await
                .map_err(|_| GitFailure::CancelledRead)?;
        }
    }

    fn schedule_read(
        &self,
        principal: AuthenticatedGitPrincipal,
        query: GitQuery,
        refresh: bool,
    ) -> Result<watch::Receiver<Option<ReadResult>>, GitFailure> {
        let mut reads = self
            .inner
            .reads
            .lock()
            .map_err(|_| GitFailure::StorageFailure)?;
        self.schedule_read_locked(&mut reads, principal, query, refresh)
    }

    fn schedule_read_locked(
        &self,
        reads: &mut ReadState,
        principal: AuthenticatedGitPrincipal,
        query: GitQuery,
        refresh: bool,
    ) -> Result<watch::Receiver<Option<ReadResult>>, GitFailure> {
        if self.inner.cancellation.is_cancelled() {
            return Err(GitFailure::CancelledRead);
        }
        reads.jobs.retain(|job| !job.handle.is_finished());
        self.schedule_pending_inventory(reads);
        if let Some(job) = reads
            .jobs
            .iter_mut()
            .find(|job| job.active && job.principal == principal && job.query == query)
        {
            job.dirty |= refresh;
            return Ok(job.receiver.clone());
        }
        let maintenance = reads
            .maintenance
            .as_ref()
            .filter(|maintenance| maintenance.owner().is_some());
        if reads.jobs.iter().filter(|job| job.active).count() + usize::from(maintenance.is_some())
            >= MAX_READ_JOBS
            || reads
                .jobs
                .iter()
                .filter(|job| job.active && job.principal == principal)
                .count()
                + usize::from(maintenance.is_some_and(|job| job.attributed_to(principal)))
                >= MAX_CLIENT_READS
        {
            return Err(GitFailure::Capacity);
        }
        let (sender, receiver) = watch::channel(None);
        let service = self.clone();
        let worker_query = query.clone();
        let handle = tokio::spawn(async move {
            let mut source = None;
            let result = service
                .run_query(principal, worker_query.clone(), &mut source)
                .await;
            // Each job channel represents one read generation. A follow-up gets a fresh channel.
            sender.send_replace(Some(ReadResult {
                result: result.clone(),
                source,
            }));
            if let GitQuery::Status { target } = worker_query {
                service.publish_status(principal, target, &result);
            }
            if let Ok(mut reads) = service.inner.reads.lock() {
                if let Some(job) = reads.jobs.iter_mut().find(|job| {
                    job.active && job.principal == principal && job.query == worker_query
                }) {
                    job.active = false;
                    if std::mem::take(&mut job.dirty)
                        && let GitQuery::Status { target } = worker_query
                    {
                        enqueue_status(&mut reads, principal, target);
                    }
                }
                let _ = service.schedule_pending_statuses(&mut reads);
            }
        });
        reads.jobs.push(ReadJob {
            principal,
            query,
            receiver: receiver.clone(),
            handle,
            active: true,
            dirty: false,
        });
        Ok(receiver)
    }

    async fn run_query(
        &self,
        principal: AuthenticatedGitPrincipal,
        query: GitQuery,
        result_source: &mut Option<(GitStatusTarget, SourceBinding)>,
    ) -> ScopedResult {
        let deadline = tokio::time::Instant::now() + READ_DEADLINE;
        let permit = self
            .read_wait(deadline, self.inner.read_slots.clone().acquire_owned())
            .await?
            .map_err(|_| GitFailure::CancelledRead)?;
        let _permit = permit;
        self.read_wait(deadline, self.inner.organization.authorize(principal))
            .await??;
        let basis = self
            .read_wait(deadline, self.inner.organization.snapshot())
            .await??;
        self.validate_snapshot(&basis)?;
        let host = Arc::clone(&self.inner.host);
        let result = match query {
            GitQuery::Status { target } => {
                let source = resolve_source(&basis, target)?;
                *result_source = Some((target, source.clone()));
                let worker_source = source.clone();
                let worker_basis = basis.clone();
                let observed = match self
                    .blocking_read(deadline, move |cancel| {
                        host.status(&worker_source.directory, &worker_basis, cancel)
                    })
                    .await
                {
                    Ok(observed) => observed,
                    Err(error) => {
                        let current = self
                            .read_wait(deadline, self.inner.organization.snapshot())
                            .await??;
                        self.validate_snapshot(&current)?;
                        if source != resolve_source(&current, target)? {
                            return Err(GitFailure::StaleAssessment);
                        }
                        self.read_wait(deadline, self.inner.organization.authorize(principal))
                            .await??;
                        return self.last_known_status(principal, target, source, error);
                    }
                };
                let current = self
                    .read_wait(deadline, self.inner.organization.snapshot())
                    .await??;
                self.validate_snapshot(&current)?;
                if source != resolve_source(&current, target)? {
                    return Err(GitFailure::StaleAssessment);
                }
                let committed = match observed.inventory {
                    Some(inventory) => {
                        validate_inventory_basis(&basis, &current, &inventory)?;
                        Some(
                            self.reconcile_observation(principal, current, inventory, deadline)
                                .await?,
                        )
                    }
                    None => {
                        self.reconcile_outside_source(
                            principal,
                            current,
                            &source.directory,
                            deadline,
                        )
                        .await?;
                        None
                    }
                };
                let scope = committed.as_ref().and_then(|(snapshot, repository)| {
                    snapshot
                        .repositories
                        .iter()
                        .find(|r| r.scope().repository == *repository)
                        .map(RepositoryLocator::scope)
                });
                let delivery = self
                    .read_wait(deadline, self.inner.organization.snapshot())
                    .await??;
                self.validate_snapshot(&delivery)?;
                if source != resolve_source(&delivery, target)? {
                    return Err(GitFailure::StaleAssessment);
                }
                let facts = self.retain_status(principal, target, source, scope, observed.facts)?;
                Ok((scope, GitQueryResult::Status(facts)))
            }
            GitQuery::Inventory { workspace, .. } => {
                let target = GitStatusTarget::Workspace(workspace);
                let captured = resolve_source(&basis, target)?;
                *result_source = Some((target, captured.clone()));
                let worker_basis = basis.clone();
                let directory = captured.directory.clone();
                let inventory = self
                    .blocking_read(deadline, move |cancel| {
                        host.inventory(&directory, &worker_basis, cancel)
                    })
                    .await?;
                let current = self
                    .read_wait(deadline, self.inner.organization.snapshot())
                    .await??;
                self.validate_snapshot(&current)?;
                if captured != resolve_source(&current, target)? {
                    return Err(GitFailure::StaleAssessment);
                }
                match inventory {
                    None => {
                        let committed = self
                            .reconcile_outside_source(
                                principal,
                                current,
                                &captured.directory,
                                deadline,
                            )
                            .await?;
                        Ok((
                            None,
                            GitQueryResult::Inventory(GitInventoryPage {
                                records: vec![],
                                next: None,
                                version: committed.organization.state().revision,
                            }),
                        ))
                    }
                    Some(inventory) => {
                        validate_inventory_basis(&basis, &current, &inventory)?;
                        let (committed, repository) = self
                            .reconcile_observation(principal, current, inventory, deadline)
                            .await?;
                        let delivery = self
                            .read_wait(deadline, self.inner.organization.snapshot())
                            .await??;
                        self.validate_snapshot(&delivery)?;
                        if captured != resolve_source(&delivery, target)? {
                            return Err(GitFailure::StaleAssessment);
                        }
                        let scope = RepositoryScope {
                            server: self.inner.server,
                            repository,
                        };
                        let facts = committed
                            .organization
                            .state()
                            .retained
                            .as_ref()
                            .ok_or(GitFailure::Unchecked)?;
                        let records = committed
                            .organization
                            .state()
                            .worktrees
                            .iter()
                            .filter(|w| w.repository == repository)
                            .map(|w| {
                                let retained = facts
                                    .worktree_inventory()
                                    .iter()
                                    .find(|v| v.worktree() == w.id)
                                    .ok_or(GitFailure::Unchecked)?;
                                Ok(GitInventoryRecord {
                                    worktree: w.id,
                                    directory: w.directory.clone(),
                                    main: w.main,
                                    missing: w.missing,
                                    listed: retained.listed(),
                                    version: retained.version(),
                                })
                            })
                            .collect::<Result<Vec<_>, GitFailure>>()?;
                        self.first_page(
                            principal,
                            scope,
                            PageData::Inventory {
                                workspace,
                                source: captured,
                                revision: committed.organization.state().revision,
                                records,
                            },
                        )
                    }
                }
            }
            GitQuery::Branches { repository, .. } => {
                let repository_locator = locator(&basis, repository)?.clone();
                let records = self
                    .blocking_read(deadline, move |cancel| {
                        host.branches(&repository_locator, cancel)
                    })
                    .await?;
                if records.len() > 16_384
                    || records.iter().map(|r| r.name.as_str().len()).sum::<usize>() > 1024 * 1024
                {
                    return Err(GitFailure::Capacity);
                }
                let current = self
                    .read_wait(deadline, self.inner.organization.snapshot())
                    .await??;
                if locator(&current, repository)? != locator(&basis, repository)? {
                    return Err(GitFailure::StaleAssessment);
                }
                self.first_page(
                    principal,
                    repository,
                    PageData::Branches {
                        records,
                        version: inventory_version(&current, repository.repository),
                    },
                )
            }
            GitQuery::PreviewCreate { request } => {
                locator(&basis, request.parts().repository)?;
                let worker_basis = basis.clone();
                let (plan, native) = self
                    .blocking_read(deadline, move |cancel| {
                        host.creation_assessment(request, &worker_basis, cancel)
                    })
                    .await?;
                let token = self
                    .cache_shared_assessment(principal, Arc::clone(&plan), native)
                    .await?;
                let (plan, residency) = self.assessment_plan(principal, &token)?;
                self.first_page(
                    principal,
                    plan.repository.scope(),
                    PageData::Assessment {
                        token,
                        plan,
                        _residency: residency,
                    },
                )
            }
            GitQuery::AssessRemove { worktree } => {
                let target = basis
                    .organization
                    .state()
                    .worktrees
                    .iter()
                    .find(|w| w.id == worktree)
                    .ok_or(GitFailure::TargetGone)?;
                let repository = locator(
                    &basis,
                    RepositoryScope {
                        server: self.inner.server,
                        repository: target.repository,
                    },
                )?
                .clone();
                let inventory_host = Arc::clone(&host);
                let inventory_basis = basis.clone();
                let inventory = self
                    .blocking_read(deadline, move |cancel| {
                        inventory_host.inventory(
                            &repository.parts().main_root,
                            &inventory_basis,
                            cancel,
                        )
                    })
                    .await?
                    .ok_or(GitFailure::UnlistedWorktree)?;
                let current = self
                    .read_wait(deadline, self.inner.organization.snapshot())
                    .await??;
                self.validate_snapshot(&current)?;
                validate_inventory_basis(&basis, &current, &inventory)?;
                // Fresh linked fallback identities belong to the actor before the
                // exact assessment is built. This never creates a Tab or starts a shell.
                let (basis, _) = self
                    .reconcile_observation(principal, current, inventory, deadline)
                    .await?;
                let worker_basis = basis.clone();
                let (plan, native) = self
                    .blocking_read(deadline, move |cancel| {
                        host.removal_assessment(worktree, &worker_basis, cancel)
                    })
                    .await?;
                let token = self
                    .cache_shared_assessment(principal, Arc::clone(&plan), native)
                    .await?;
                let (plan, residency) = self.assessment_plan(principal, &token)?;
                self.first_page(
                    principal,
                    plan.repository.scope(),
                    PageData::Assessment {
                        token,
                        plan,
                        _residency: residency,
                    },
                )
            }
            GitQuery::AssessmentPage { .. } => Err(GitFailure::InvalidResponse),
        };
        self.read_wait(deadline, self.inner.organization.authorize(principal))
            .await??;
        if let Ok((scope, result)) = &result {
            result.validate(*scope, self.inner.server)?;
        }
        result
    }

    async fn read_wait<T>(
        &self,
        deadline: tokio::time::Instant,
        future: impl std::future::Future<Output = T>,
    ) -> Result<T, GitFailure> {
        let cancelled = self.inner.read_wake.notified();
        tokio::pin!(cancelled);
        cancelled.as_mut().enable();
        if self.inner.cancellation.is_cancelled() {
            return Err(GitFailure::CancelledRead);
        }
        tokio::select! {
            result = future => Ok(result),
            _ = tokio::time::sleep_until(deadline) => Err(GitFailure::TimedOutRead),
            _ = cancelled => Err(GitFailure::CancelledRead),
        }
    }

    async fn blocking_read<T: Send + 'static>(
        &self,
        deadline: tokio::time::Instant,
        run: impl FnOnce(&GitReadCancellation) -> Result<T, GitFailure> + Send + 'static,
    ) -> Result<T, GitFailure> {
        let cancellation =
            GitReadCancellation::linked(&self.inner.cancellation, &GitReadCancellation::default());
        let worker_cancel = cancellation.clone();
        let mut worker = tokio::task::spawn_blocking(move || run(&worker_cancel));
        match self.read_wait(deadline, &mut worker).await {
            Ok(result) => result.map_err(|_| GitFailure::Unchecked)?,
            Err(error) => {
                cancellation.cancel();
                // A deadline requests cancellation, not detachment. Reap the owned child first.
                let _ = worker.await;
                Err(error)
            }
        }
    }

    fn validate_snapshot(&self, snapshot: &GitAuthoritySnapshot) -> Result<(), GitFailure> {
        if snapshot.organization.state().server != self.inner.server
            || snapshot.home.server() != self.inner.server
        {
            return Err(GitFailure::WrongServer);
        }
        if snapshot.organization.epoch() != self.inner.epoch {
            return Err(GitFailure::StaleAssessment);
        }
        if snapshot.repositories.len() > MAX_GIT_REPOSITORIES
            || snapshot.display_directories.len() > MAX_GIT_PANES
            || snapshot.runtime.panes.len() > MAX_GIT_PANES
        {
            return Err(GitFailure::Capacity);
        }
        if snapshot
            .repositories
            .iter()
            .any(|r| r.scope().server != self.inner.server)
        {
            return Err(GitFailure::WrongServer);
        }
        for stamp in &snapshot.runtime.panes {
            if let Some(observation) = &stamp.confirmed_directory {
                let pane = snapshot
                    .organization
                    .state()
                    .panes
                    .iter()
                    .find(|p| p.id == stamp.pane)
                    .ok_or(GitFailure::TargetGone)?;
                if observation.session().epoch != self.inner.epoch
                    || observation.session().pane != stamp.pane
                    || stamp.session != Some(observation.session())
                    || pane.confirmed_restoration_directory.value.as_ref()
                        != Some(observation.directory())
                {
                    return Err(GitFailure::StaleAssessment);
                }
            }
        }
        Ok(())
    }

    async fn reconcile_observation(
        &self,
        principal: AuthenticatedGitPrincipal,
        basis: GitAuthoritySnapshot,
        inventory: HostGitInventory,
        deadline: tokio::time::Instant,
    ) -> Result<(GitAuthoritySnapshot, RepositoryId), GitFailure> {
        if inventory.main_root.server() != self.inner.server
            || inventory.common_directory.server() != self.inner.server
        {
            return Err(GitFailure::WrongServer);
        }
        if inventory.records.is_empty() || inventory.records.len() > MAX_GIT_REPOSITORIES {
            return Err(GitFailure::Capacity);
        }
        validate_inventory(&basis, &inventory)?;
        let host = Arc::clone(&self.inner.host);
        let worker_basis = basis.clone();
        let worker_inventory = inventory.clone();
        let memberships = self
            .blocking_read(deadline, move |cancel| {
                inventory_memberships(&*host, &worker_basis, &worker_inventory, cancel)
            })
            .await?;
        let lease = inventory.lease.clone();
        if let Some(lease) = &lease {
            lease.revalidate()?;
        }
        let main_root = inventory.main_root.clone();
        let common_directory = inventory.common_directory.clone();
        let committed = self
            .read_wait(
                deadline,
                self.inner.organization.reconcile_inventory(
                    principal,
                    GitInventoryReconciliation {
                        basis,
                        inventory,
                        memberships,
                    },
                ),
            )
            .await?;
        let committed = match committed {
            Ok(committed) => committed,
            Err(GitFailure::IndeterminateDurability) => {
                if let Ok(mut state) = self.inner.state.lock() {
                    state.uncertain = true;
                }
                self.inner.organization.close_mutation_admission().await;
                return Err(GitFailure::IndeterminateDurability);
            }
            Err(error) => return Err(error),
        };
        self.validate_snapshot(&committed)?;
        let repository = committed
            .repositories
            .iter()
            .find(|r| {
                r.parts().main_root == main_root && r.parts().common_directory == common_directory
            })
            .ok_or(GitFailure::InvalidResponse)?
            .scope()
            .repository;
        if let Some(lease) = lease {
            lease.retain_committed()?;
        }
        Ok((committed, repository))
    }

    async fn reconcile_outside_source(
        &self,
        principal: AuthenticatedGitPrincipal,
        basis: GitAuthoritySnapshot,
        directory: &ServerDirectory,
        deadline: tokio::time::Instant,
    ) -> Result<GitAuthoritySnapshot, GitFailure> {
        let state = basis.organization.state();
        let facts = state.retained.as_ref().ok_or(GitFailure::Unchecked)?;
        let mut memberships = Vec::new();
        for workspace in state
            .workspaces
            .iter()
            .filter(|w| w.pinned_directory.value.is_none())
        {
            if resolve_source(&basis, GitStatusTarget::Workspace(workspace.id))?.directory
                != *directory
            {
                continue;
            }
            let root = state
                .tabs
                .iter()
                .find(|t| t.id == workspace.root_tab)
                .ok_or(GitFailure::Unchecked)?;
            if root.worktree.is_some() {
                memberships.push(WorktreeMembershipEdit {
                    tab: root.id,
                    expected_version: facts
                        .tab_membership_versions()
                        .iter()
                        .find(|v| v.id() == root.id)
                        .ok_or(GitFailure::Unchecked)?
                        .version(),
                    worktree: None,
                });
            }
        }
        if memberships.is_empty() {
            return Ok(basis);
        }
        let result = self
            .read_wait(
                deadline,
                self.inner.organization.reconcile_outside_source(
                    principal,
                    GitOutsideSourceReconciliation {
                        basis,
                        directory: directory.clone(),
                        memberships,
                    },
                ),
            )
            .await?;
        let committed = match result {
            Err(GitFailure::IndeterminateDurability) => {
                if let Ok(mut state) = self.inner.state.lock() {
                    state.uncertain = true;
                }
                self.inner.organization.close_mutation_admission().await;
                return Err(GitFailure::IndeterminateDurability);
            }
            result => result?,
        };
        self.validate_snapshot(&committed)?;
        Ok(committed)
    }

    fn retain_status(
        &self,
        principal: AuthenticatedGitPrincipal,
        target: GitStatusTarget,
        source: SourceBinding,
        scope: Option<RepositoryScope>,
        mut facts: GitStatusFacts,
    ) -> Result<GitStatusFacts, GitFailure> {
        self.expire_read_cache()?;
        let mut reads = self
            .inner
            .reads
            .lock()
            .map_err(|_| GitFailure::StorageFailure)?;
        if self.inner.cancellation.is_cancelled() {
            return Err(GitFailure::CancelledRead);
        }
        reads.revision = reads.revision.checked_add(1).ok_or(GitFailure::Capacity)?;
        facts.revision = NonZeroU64::new(reads.revision).ok_or(GitFailure::Capacity)?;
        GitQueryResult::Status(facts.clone()).validate(scope, self.inner.server)?;
        reads
            .statuses
            .retain(|entry| !(entry.principal == principal && entry.target == target));
        if reads
            .statuses
            .iter()
            .filter(|entry| entry.principal == principal)
            .count()
            >= 32
            && let Some(index) = reads
                .statuses
                .iter()
                .position(|entry| entry.principal == principal)
        {
            reads.statuses.remove(index);
        }
        // Status is replaceable. Eviction never touches durable inventory, assessments or outcomes.
        while reads.statuses.len() >= 128 {
            reads.statuses.remove(0);
        }
        let residency = loop {
            match ReadReservation::reserve(
                &reads.residency,
                status_fact_bytes(&facts) + source.directory.bytes().len(),
            ) {
                Ok(reservation) => break reservation,
                Err(GitFailure::Capacity) if !reads.statuses.is_empty() => {
                    reads.statuses.remove(0);
                }
                Err(error) => return Err(error),
            }
        };
        reads.statuses.push(CachedStatus {
            principal,
            target,
            source,
            scope,
            facts: facts.clone(),
            updated: Instant::now(),
            _residency: residency,
        });
        Ok(facts)
    }

    fn last_known_status(
        &self,
        principal: AuthenticatedGitPrincipal,
        target: GitStatusTarget,
        source: SourceBinding,
        failure: GitFailure,
    ) -> ScopedResult {
        if !matches!(
            failure,
            GitFailure::Unchecked
                | GitFailure::ToolMissing
                | GitFailure::ToolTooOld
                | GitFailure::TimedOutRead
                | GitFailure::InvalidResponse
                | GitFailure::OutputTooLarge
                | GitFailure::InvalidDirectory
                | GitFailure::IdentityReplaced
                | GitFailure::GitStateChanged
        ) {
            return Err(failure);
        }
        let cached = {
            let reads = self
                .inner
                .reads
                .lock()
                .map_err(|_| GitFailure::StorageFailure)?;
            reads
                .statuses
                .iter()
                .find(|s| {
                    s.principal == principal
                        && s.target == target
                        && s.source == source
                        && s.updated.elapsed() < CACHE_LIFETIME
                })
                .map(|s| (s.scope, s.facts.clone()))
        };
        let Some((scope, mut facts)) = cached else {
            return Err(failure);
        };
        facts.unchecked = true;
        let facts = self.retain_status(principal, target, source, scope, facts)?;
        Ok((scope, GitQueryResult::Status(facts)))
    }

    fn publish_status(
        &self,
        principal: AuthenticatedGitPrincipal,
        target: GitStatusTarget,
        result: &ScopedResult,
    ) {
        let Ok(mut reads) = self.inner.reads.lock() else {
            return;
        };
        let (scope, result) = match result {
            Ok((scope, GitQueryResult::Status(facts))) => (*scope, Ok(facts.clone())),
            Err(error) => (None, Err(*error)),
            _ => return,
        };
        let source = result.as_ref().ok().and_then(|facts| {
            reads
                .statuses
                .iter()
                .find(|entry| {
                    entry.principal == principal
                        && entry.target == target
                        && entry.facts.revision == facts.revision
                })
                .map(|entry| entry.source.clone())
        });
        if result.is_ok() && source.is_none() {
            return;
        }
        let update = GitStatusUpdate {
            epoch: self.inner.epoch,
            target,
            scope,
            result,
            source,
        };
        reads
            .interests
            .retain(|interest| interest.sender.receiver_count() != 0);
        for interest in &reads.interests {
            if interest.principal == principal && interest.target == target {
                interest.sender.send_replace(Some(update.clone()));
            }
        }
    }

    pub async fn subscribe_status(
        &self,
        principal: AuthenticatedGitPrincipal,
        target: GitStatusTarget,
    ) -> Result<GitStatusSubscription, GitFailure> {
        self.check_principal(principal)?;
        GitQuery::Status { target }.validate_scope(self.inner.server)?;
        self.admission()?;
        self.inner.organization.authorize(principal).await?;
        let receiver = {
            let mut reads = self
                .inner
                .reads
                .lock()
                .map_err(|_| GitFailure::StorageFailure)?;
            if self.inner.cancellation.is_cancelled() {
                return Err(GitFailure::CancelledRead);
            }
            reads
                .interests
                .retain(|interest| interest.sender.receiver_count() != 0);
            if let Some(interest) = reads
                .interests
                .iter()
                .find(|interest| interest.principal == principal && interest.target == target)
            {
                interest.sender.subscribe()
            } else {
                if reads.interests.len() >= MAX_READ_JOBS
                    || reads
                        .interests
                        .iter()
                        .filter(|i| i.principal == principal)
                        .count()
                        >= 8
                {
                    return Err(GitFailure::Capacity);
                }
                let (sender, receiver) = watch::channel(None);
                reads.interests.push(StatusInterest {
                    principal,
                    target,
                    sender,
                });
                receiver
            }
        };
        match self.schedule_read(principal, GitQuery::Status { target }, false) {
            Err(GitFailure::Capacity) => {
                let mut reads = self
                    .inner
                    .reads
                    .lock()
                    .map_err(|_| GitFailure::StorageFailure)?;
                enqueue_status(&mut reads, principal, target);
                self.schedule_pending_statuses(&mut reads)?;
            }
            result => {
                result?;
            }
        }
        self.start_read_poller(principal)?;
        Ok(GitStatusSubscription {
            receiver,
            delivered: false,
        })
    }

    fn start_read_poller(&self, principal: AuthenticatedGitPrincipal) -> Result<(), GitFailure> {
        let mut reads = self
            .inner
            .reads
            .lock()
            .map_err(|_| GitFailure::StorageFailure)?;
        if self.inner.cancellation.is_cancelled() {
            return Err(GitFailure::CancelledRead);
        }
        if !reads.principals.contains(&principal) {
            if reads.principals.len() == 32 {
                reads.principals.remove(0);
            }
            reads.principals.push(principal);
        }
        if reads.poller.is_none() {
            let service = self.clone();
            reads.poller = Some(tokio::spawn(async move {
                let mut interval = tokio::time::interval(REFRESH_INTERVAL);
                interval.tick().await;
                loop {
                    if service
                        .read_wait(
                            tokio::time::Instant::now() + REFRESH_INTERVAL * 2,
                            interval.tick(),
                        )
                        .await
                        .is_err()
                    {
                        break;
                    }
                    if service.refresh_statuses().is_err() {
                        break;
                    }
                    let principals = match service.inner.reads.lock() {
                        Ok(reads) => reads.principals.clone(),
                        Err(_) => break,
                    };
                    for principal in principals {
                        if service
                            .read_wait(
                                tokio::time::Instant::now() + READ_DEADLINE,
                                service.inner.organization.authorize(principal),
                            )
                            .await
                            == Ok(Ok(()))
                        {
                            let _ = service.refresh_inventory(principal);
                            break;
                        }
                    }
                }
            }));
        }
        Ok(())
    }

    /// Refresh all retained Workspace sources and repository roots, independent
    /// of subscriptions, attachments and active views. The authenticated identity
    /// remains stable across disconnect; the actor rechecks revocation each pass.
    /// Server-core calls this after committed directory or Worktree changes too.
    /// Capacity retains one dirty request for retry when a read slot becomes available.
    pub fn refresh_inventory(
        &self,
        principal: AuthenticatedGitPrincipal,
    ) -> Result<(), GitFailure> {
        self.check_principal(principal)?;
        self.admission()?;
        let mut reads = self
            .inner
            .reads
            .lock()
            .map_err(|_| GitFailure::StorageFailure)?;
        if self.inner.cancellation.is_cancelled() {
            return Err(GitFailure::CancelledRead);
        }
        if reads.maintenance.is_none() {
            let service = self.clone();
            let handle = tokio::spawn(async move {
                loop {
                    service.inner.inventory_wake.notified().await;
                    if service.inner.cancellation.is_cancelled() {
                        break;
                    }
                    let principal = {
                        let Ok(mut reads) = service.inner.reads.lock() else {
                            break;
                        };
                        let Some(maintenance) = &mut reads.maintenance else {
                            break;
                        };
                        if !maintenance.pending_reserved {
                            continue;
                        }
                        let principal = maintenance.pending.take();
                        maintenance.pending_reserved = false;
                        maintenance.active = principal;
                        principal
                    };
                    let Some(principal) = principal else {
                        continue;
                    };
                    let _ = service.refresh_inventory_batch(principal).await;
                    let Ok(mut reads) = service.inner.reads.lock() else {
                        break;
                    };
                    if let Some(maintenance) = &mut reads.maintenance {
                        maintenance.active = None;
                    }
                    let _ = service.schedule_pending_statuses(&mut reads);
                }
            });
            reads.maintenance = Some(InventoryMaintenance {
                handle,
                pending: None,
                pending_reserved: false,
                active: None,
            });
        }
        if let Some(maintenance) = &mut reads.maintenance {
            maintenance.pending = Some(principal);
            maintenance.pending_reserved = false;
        }
        if self.schedule_pending_inventory(&mut reads) {
            Ok(())
        } else {
            Err(GitFailure::Capacity)
        }
    }

    fn schedule_pending_inventory(&self, reads: &mut ReadState) -> bool {
        if self.inner.cancellation.is_cancelled() {
            return false;
        }
        let Some(maintenance) = &mut reads.maintenance else {
            return false;
        };
        let Some(principal) = maintenance.pending else {
            return false;
        };
        if !maintenance.pending_reserved {
            // A full quota retains the dirty request without charging an unadmitted read.
            if (maintenance.active.is_none()
                && reads.jobs.iter().filter(|job| job.active).count() >= MAX_READ_JOBS)
                || (maintenance.active != Some(principal)
                    && reads
                        .jobs
                        .iter()
                        .filter(|job| job.active && job.principal == principal)
                        .count()
                        >= MAX_CLIENT_READS)
            {
                return false;
            }
            maintenance.pending_reserved = true;
        }
        if maintenance.active.is_none() {
            self.inner.inventory_wake.notify_one();
        }
        true
    }

    async fn refresh_inventory_batch(
        &self,
        principal: AuthenticatedGitPrincipal,
    ) -> Result<(), GitFailure> {
        let deadline = tokio::time::Instant::now() + READ_DEADLINE;
        self.read_wait(deadline, self.inner.organization.authorize(principal))
            .await??;
        let _permit = self
            .read_wait(deadline, self.inner.read_slots.clone().acquire_owned())
            .await?
            .map_err(|_| GitFailure::CancelledRead)?;
        let basis = self
            .read_wait(deadline, self.inner.organization.snapshot())
            .await??;
        self.validate_snapshot(&basis)?;
        let mut directories = Vec::new();
        for workspace in &basis.organization.state().workspaces {
            let directory =
                resolve_source(&basis, GitStatusTarget::Workspace(workspace.id))?.directory;
            if !directories.contains(&directory) {
                directories.push(directory);
            }
        }
        for repository in &basis.repositories {
            if !directories.contains(&repository.parts().main_root) {
                directories.push(repository.parts().main_root.clone());
            }
        }
        if directories.is_empty() {
            return Ok(());
        }
        let offset = {
            let mut reads = self
                .inner
                .reads
                .lock()
                .map_err(|_| GitFailure::StorageFailure)?;
            let offset = reads.inventory_offset % directories.len();
            reads.inventory_offset =
                (offset + MAX_READ_JOBS.min(directories.len())) % directories.len();
            offset
        };
        for directory in directories
            .iter()
            .cycle()
            .skip(offset)
            .take(MAX_READ_JOBS.min(directories.len()))
        {
            let deadline = tokio::time::Instant::now() + READ_DEADLINE;
            self.read_wait(deadline, self.inner.organization.authorize(principal))
                .await??;
            let current = self
                .read_wait(deadline, self.inner.organization.snapshot())
                .await??;
            self.validate_snapshot(&current)?;
            let worker_basis = current.clone();
            let selected = directory.clone();
            let host = Arc::clone(&self.inner.host);
            match self
                .blocking_read(deadline, move |cancel| {
                    host.inventory(&selected, &worker_basis, cancel)
                })
                .await
            {
                Ok(Some(inventory)) => {
                    let _ = self
                        .reconcile_observation(principal, current, inventory, deadline)
                        .await;
                }
                Ok(None) => {
                    let _ = self
                        .reconcile_outside_source(principal, current, directory, deadline)
                        .await;
                }
                Err(GitFailure::CancelledRead) if self.inner.cancellation.is_cancelled() => {
                    return Err(GitFailure::CancelledRead);
                }
                _ => {} // Failed reads leave every durable record and membership intact.
            }
        }
        Ok(())
    }

    /// Coalesce explicit/native refresh triggers; at most one follow-up is dirty
    /// while a target is being read. Interest never depends on an active window.
    pub fn refresh_statuses(&self) -> Result<(), GitFailure> {
        self.admission()?;
        let mut reads = self
            .inner
            .reads
            .lock()
            .map_err(|_| GitFailure::StorageFailure)?;
        reads.interests.retain(|i| i.sender.receiver_count() != 0);
        let interests = reads
            .interests
            .iter()
            .map(|i| (i.principal, i.target))
            .collect::<Vec<_>>();
        for (principal, target) in interests {
            enqueue_status(&mut reads, principal, target);
        }
        self.schedule_pending_statuses(&mut reads)
    }

    fn schedule_pending_statuses(&self, reads: &mut ReadState) -> Result<(), GitFailure> {
        // Authoritative inventory takes freed capacity before replaceable status follow-ups.
        self.schedule_pending_inventory(reads);
        reads.pending_statuses.retain(|(principal, target)| {
            reads.interests.iter().any(|i| {
                i.principal == *principal && i.target == *target && i.sender.receiver_count() != 0
            })
        });
        // Deferred targets stay ahead of follow-ups and subsequent refresh triggers.
        // One bounded pass also lets another principal run when only one principal is full.
        for _ in 0..reads.pending_statuses.len() {
            let Some((principal, target)) = reads.pending_statuses.pop_front() else {
                break;
            };
            match self.schedule_read_locked(reads, principal, GitQuery::Status { target }, true) {
                Ok(_) => {}
                Err(GitFailure::Capacity) => reads.pending_statuses.push_back((principal, target)),
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    pub(crate) async fn shutdown_reads(&self) -> Result<(), GitFailure> {
        self.inner.read_wake.notify_waiters();
        let (jobs, poller, maintenance) = {
            let mut reads = self
                .inner
                .reads
                .lock()
                .map_err(|_| GitFailure::StorageFailure)?;
            for interest in &reads.interests {
                interest.sender.send_replace(Some(GitStatusUpdate {
                    epoch: self.inner.epoch,
                    target: interest.target,
                    scope: None,
                    result: Err(GitFailure::CancelledRead),
                    source: None,
                }));
            }
            reads.interests.clear();
            reads.pending_statuses.clear();
            reads.pages.clear();
            reads.statuses.clear();
            (
                std::mem::take(&mut reads.jobs),
                reads.poller.take(),
                reads.maintenance.take(),
            )
        };
        let mut failed = false;
        if let Some(poller) = poller {
            failed |= poller.await.is_err();
        }
        if let Some(maintenance) = maintenance {
            failed |= maintenance.handle.await.is_err();
        }
        for job in jobs {
            failed |= job.handle.await.is_err();
        }
        if failed {
            Err(GitFailure::Unchecked)
        } else {
            Ok(())
        }
    }

    fn assessment_plan(
        &self,
        principal: AuthenticatedGitPrincipal,
        token: &GitAssessmentToken,
    ) -> Result<(Arc<ConfirmedGitPlan>, Arc<ReadReservation>), GitFailure> {
        check_token(principal, token)?;
        self.expire_read_cache()?;
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| GitFailure::StorageFailure)?;
        let cached = state
            .assessments
            .get(&token.parts().random)
            .ok_or(GitFailure::ExpiredAssessment)?;
        if cached.principal != principal || cached.token != *token {
            return Err(GitFailure::Unauthorized);
        }
        if cached.created.elapsed() >= CACHE_LIFETIME {
            return Err(GitFailure::ExpiredAssessment);
        }
        Ok((cached.plan.clone(), cached.residency.clone()))
    }

    fn first_page(
        &self,
        principal: AuthenticatedGitPrincipal,
        scope: RepositoryScope,
        data: PageData,
    ) -> ScopedResult {
        self.expire_read_cache()?;
        let mut reads = self
            .inner
            .reads
            .lock()
            .map_err(|_| GitFailure::StorageFailure)?;
        if self.inner.cancellation.is_cancelled() {
            return Err(GitFailure::CancelledRead);
        }
        if matches!(data, PageData::Branches { .. }) {
            reads.pages.retain(|page| {
                page.principal != principal
                    || page.scope != scope
                    || !matches!(page.data, PageData::Branches { .. })
            });
        }
        reads.observation = reads
            .observation
            .checked_add(1)
            .ok_or(GitFailure::Capacity)?;
        let observation = match &data {
            PageData::Inventory { revision, .. } => revision.get(),
            _ => reads.observation,
        };
        let mut page = PageObservation {
            principal,
            scope,
            observation,
            data,
            cursors: BTreeMap::new(),
            residency: None,
            created: Instant::now(),
        };
        let result = produce_page(&mut page, 0, self.inner.epoch)?;
        result.validate(Some(scope), self.inner.server)?;
        if result_cursor(&result).is_some() {
            if reads.pages.len() >= 24
                || reads
                    .pages
                    .iter()
                    .filter(|p| p.principal == principal)
                    .count()
                    >= 8
            {
                return Err(GitFailure::Capacity);
            }
            page.residency = Some(ReadReservation::reserve(&reads.residency, page.bytes())?);
            reads.pages.push(page);
        }
        Ok((Some(scope), result))
    }

    async fn continue_page(
        &self,
        principal: AuthenticatedGitPrincipal,
        query: &GitQuery,
        cursor: &GitPageCursor,
    ) -> ScopedResult {
        if cursor.parts().epoch != self.inner.epoch {
            return Err(GitFailure::StaleAssessment);
        }
        let deadline = tokio::time::Instant::now() + READ_DEADLINE;
        let basis = self
            .read_wait(deadline, self.inner.organization.snapshot())
            .await??;
        self.validate_snapshot(&basis)?;
        self.read_wait(deadline, self.inner.organization.authorize(principal))
            .await??;
        if let GitQuery::AssessmentPage { assessment, .. } = query {
            self.assessment_plan(principal, assessment)?;
        }
        self.expire_read_cache()?;
        let mut reads = self
            .inner
            .reads
            .lock()
            .map_err(|_| GitFailure::StorageFailure)?;
        if self.inner.cancellation.is_cancelled() {
            return Err(GitFailure::CancelledRead);
        }
        let page = reads
            .pages
            .iter_mut()
            .find(|p| p.cursors.contains_key(&cursor.parts().random))
            .ok_or(GitFailure::StaleAssessment)?;
        if page.principal != principal {
            return Err(GitFailure::Unauthorized);
        }
        if page.scope != cursor.parts().scope || page.observation != cursor.parts().observation {
            return Err(GitFailure::StaleAssessment);
        }
        match (&page.data, query) {
            (
                PageData::Inventory {
                    workspace,
                    source,
                    revision,
                    ..
                },
                GitQuery::Inventory {
                    workspace: actual, ..
                },
            ) if workspace == actual
                && *revision == basis.organization.state().revision
                && *source == resolve_source(&basis, GitStatusTarget::Workspace(*workspace))? => {}
            (PageData::Branches { version, .. }, GitQuery::Branches { repository, .. })
                if *repository == page.scope
                    && *version == inventory_version(&basis, repository.repository) =>
            {
                locator(&basis, *repository)?;
            }
            (PageData::Assessment { token, .. }, GitQuery::AssessmentPage { assessment, .. })
                if token == assessment => {}
            _ => return Err(GitFailure::StaleAssessment),
        }
        let offset = *page
            .cursors
            .get(&cursor.parts().random)
            .ok_or(GitFailure::StaleAssessment)?;
        let result = produce_page(page, offset, self.inner.epoch)?;
        result.validate(Some(page.scope), self.inner.server)?;
        Ok((Some(page.scope), result))
    }

    /// Internal host/actor pipeline only. Plans and native proofs are never wire input.
    pub async fn cache_assessment(
        &self,
        principal: AuthenticatedGitPrincipal,
        plan: ConfirmedGitPlan,
        native: Arc<dyn GitHostAssessment>,
    ) -> Result<GitAssessmentToken, GitFailure> {
        self.cache_shared_assessment(principal, Arc::new(plan), native)
            .await
    }
    async fn cache_shared_assessment(
        &self,
        principal: AuthenticatedGitPrincipal,
        plan: Arc<ConfirmedGitPlan>,
        native: Arc<dyn GitHostAssessment>,
    ) -> Result<GitAssessmentToken, GitFailure> {
        self.check_principal(principal)?;
        plan.validate_scope()?;
        self.admission()?;
        self.inner.organization.authorize(principal).await?;
        if plan.repository.scope().server != self.inner.server {
            return Err(GitFailure::WrongServer);
        }
        let bytes = assessment_bytes(&plan);
        self.expire_read_cache()?;
        let residency_pool = self
            .inner
            .reads
            .lock()
            .map_err(|_| GitFailure::StorageFailure)?
            .residency
            .clone();
        let mut random = [0; 32];
        getrandom::fill(&mut random).map_err(|_| GitFailure::Unchecked)?;
        let token = GitAssessmentToken::new(GitAssessmentTokenParts {
            server: self.inner.server,
            epoch: self.inner.epoch,
            pairing: principal.pairing(),
            client: principal.client(),
            kind: plan.kind(),
            random,
        })?;
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| GitFailure::StorageFailure)?;
        if !state.ready || state.closing || state.uncertain {
            return Err(GitFailure::Busy);
        }
        if state.assessments.len() >= self.inner.limits.assessments
            || state
                .assessments
                .values()
                .filter(|entry| entry.principal == principal)
                .count()
                >= self.inner.limits.assessments_per_client
        {
            return Err(GitFailure::Capacity);
        }
        if state.assessments.contains_key(&random) {
            return Err(GitFailure::Unchecked);
        }
        let residency = Arc::new(ReadReservation::reserve(&residency_pool, bytes)?);
        state.assessments.insert(
            random,
            CachedAssessment {
                token: token.clone(),
                principal,
                plan,
                native,
                created: Instant::now(),
                residency,
            },
        );
        Ok(token)
    }
}

fn enqueue_status(
    reads: &mut ReadState,
    principal: AuthenticatedGitPrincipal,
    target: GitStatusTarget,
) {
    let key = (principal, target);
    if !reads.pending_statuses.contains(&key)
        && reads.interests.iter().any(|i| {
            i.principal == principal && i.target == target && i.sender.receiver_count() != 0
        })
    {
        reads.pending_statuses.push_back(key);
    }
}

fn resolve_source(
    snapshot: &GitAuthoritySnapshot,
    target: GitStatusTarget,
) -> Result<SourceBinding, GitFailure> {
    let state = snapshot.organization.state();
    match target {
        GitStatusTarget::Pane(id) => {
            let pane = state
                .panes
                .iter()
                .find(|p| p.id == id)
                .ok_or(GitFailure::TargetGone)?;
            let runtime = snapshot
                .organization
                .runtime()
                .iter()
                .find(|p| p.pane == id)
                .ok_or(GitFailure::Unchecked)?;
            let display = snapshot
                .display_directories
                .iter()
                .filter(|d| {
                    d.session().pane == id
                        && d.session().epoch == snapshot.organization.epoch()
                        && d.session().generation == runtime.session_generation
                        && d.presentation() == DirectoryPresentation::OwningServer
                        && matches!(
                            runtime.phase,
                            spaceterm_core::organization::SessionPhase::Running { .. }
                        )
                })
                .max_by_key(|d| d.sequence());
            Ok(SourceBinding {
                directory: display.map_or_else(
                    || pane.restoration_directory().clone(),
                    |d| d.directory().clone(),
                ),
                pane: Some(id),
                session: runtime.session_generation,
                sequence: display.map(|d| d.sequence()),
                version: pane.confirmed_restoration_directory.version,
            })
        }
        GitStatusTarget::Workspace(id) => {
            let workspace = state
                .workspaces
                .iter()
                .find(|w| w.id == id)
                .ok_or(GitFailure::TargetGone)?;
            if let Some(directory) = &workspace.pinned_directory.value {
                return Ok(SourceBinding {
                    directory: directory.clone(),
                    pane: None,
                    session: SessionGeneration::new(0),
                    sequence: None,
                    version: workspace.pinned_directory.version,
                });
            }
            let root = state
                .tabs
                .iter()
                .find(|t| t.id == workspace.root_tab)
                .ok_or(GitFailure::Unchecked)?;
            resolve_source(snapshot, GitStatusTarget::Pane(root.root_pane))
        }
        GitStatusTarget::Worktree(id) => {
            let worktree = state
                .worktrees
                .iter()
                .find(|w| w.id == id)
                .ok_or(GitFailure::TargetGone)?;
            Ok(SourceBinding {
                directory: worktree.directory.clone(),
                pane: None,
                session: SessionGeneration::new(0),
                sequence: None,
                version: state
                    .retained
                    .as_ref()
                    .and_then(|f| f.worktree_inventory().iter().find(|v| v.worktree() == id))
                    .map_or(FieldVersion::new(0), |v| v.version()),
            })
        }
    }
}
fn locator(
    snapshot: &GitAuthoritySnapshot,
    scope: RepositoryScope,
) -> Result<&RepositoryLocator, GitFailure> {
    snapshot
        .repositories
        .iter()
        .find(|r| {
            r.scope() == scope
                && snapshot
                    .organization
                    .state()
                    .repositories
                    .iter()
                    .any(|record| record.id == scope.repository)
        })
        .ok_or(GitFailure::TargetGone)
}
fn inventory_version(
    snapshot: &GitAuthoritySnapshot,
    repository: RepositoryId,
) -> Option<FieldVersion> {
    snapshot
        .organization
        .state()
        .retained
        .as_ref()?
        .repository_inventory_versions()
        .iter()
        .find(|v| v.id() == repository)
        .map(|v| v.version())
}
pub(crate) fn inventory_memberships(
    host: &dyn ServerGitHost,
    basis: &GitAuthoritySnapshot,
    inventory: &HostGitInventory,
    cancellation: &GitReadCancellation,
) -> Result<Vec<GitInventoryMembership>, GitFailure> {
    let state = basis.organization.state();
    let facts = state.retained.as_ref().ok_or(GitFailure::Unchecked)?;
    let mut edits = Vec::new();
    for workspace in &state.workspaces {
        if cancellation.is_cancelled() {
            return Err(GitFailure::CancelledRead);
        }
        let directory = resolve_source(basis, GitStatusTarget::Workspace(workspace.id))?.directory;
        let destination = if directory == inventory.source {
            inventory.current.filter(|index| {
                inventory
                    .records
                    .get(*index)
                    .is_some_and(|w| !w.bare && !w.missing)
            })
        } else {
            match host.discover_source(&directory, basis, cancellation) {
                Ok(Some(source)) if source.common_directory == inventory.common_directory => Some(
                    inventory
                        .records
                        .iter()
                        .position(|w| w.directory == source.worktree && !w.bare && !w.missing)
                        .ok_or(GitFailure::GitStateChanged)?,
                ),
                Ok(Some(_)) | Err(_) => continue, // A different repository or failed read grants no membership edit.
                Ok(None) => None,
            }
        };
        for tab in state.tabs.iter().filter(|t| t.workspace == workspace.id) {
            let follows_root =
                workspace.pinned_directory.value.is_none() && workspace.root_tab == tab.id;
            let in_repository = tab.worktree.is_some_and(|id| {
                state
                    .worktrees
                    .iter()
                    .any(|w| w.id == id && Some(w.repository) == inventory.repository)
            });
            if (tab.worktree.is_none() && destination.is_some())
                || (follows_root && (destination.is_some() || in_repository))
            {
                let expected_version = facts
                    .tab_membership_versions()
                    .iter()
                    .find(|v| v.id() == tab.id)
                    .ok_or(GitFailure::Unchecked)?
                    .version();
                edits.push(GitInventoryMembership {
                    tab: tab.id,
                    expected_version,
                    destination,
                });
            }
        }
    }
    Ok(edits)
}
fn check_token(
    principal: AuthenticatedGitPrincipal,
    token: &GitAssessmentToken,
) -> Result<(), GitFailure> {
    if token.server() != principal.server() {
        return Err(GitFailure::WrongServer);
    }
    if token.epoch() != principal.epoch() {
        return Err(GitFailure::StaleAssessment);
    }
    if token.parts().pairing != principal.pairing() || token.parts().client != principal.client() {
        return Err(GitFailure::Unauthorized);
    }
    Ok(())
}
fn assessment_bytes(plan: &ConfirmedGitPlan) -> usize {
    2048 + plan.repository.parts().main_root.bytes().len()
        + plan.repository.parts().common_directory.bytes().len()
        + match &plan.action {
            ConfirmedGitAction::Create { preview, .. } => {
                4096 + preview.parts().directory.bytes().len()
            }
            ConfirmedGitAction::Remove(scope) => scope.value_bytes(),
        }
}
fn status_fact_bytes(facts: &GitStatusFacts) -> usize {
    8192 + facts
        .changes
        .iter()
        .map(|c| 64 + c.path().len())
        .sum::<usize>()
}
fn query_cursor(query: &GitQuery) -> Option<&GitPageCursor> {
    match query {
        GitQuery::Inventory { cursor, .. }
        | GitQuery::Branches { cursor, .. }
        | GitQuery::AssessmentPage { cursor, .. } => cursor.as_ref(),
        _ => None,
    }
}
fn result_cursor(result: &GitQueryResult) -> Option<&GitPageCursor> {
    match result {
        GitQueryResult::Inventory(page) => page.next.as_ref(),
        GitQueryResult::Branches(page) => page.next.as_ref(),
        GitQueryResult::Assessment(page) => page.next.as_ref(),
        _ => None,
    }
}
fn next_cursor(
    page: &mut PageObservation,
    offset: usize,
    epoch: spaceterm_core::authority::ServerEpoch,
) -> Result<GitPageCursor, GitFailure> {
    let random = match page.cursors.iter().find(|(_, old)| **old == offset) {
        Some((random, _)) => *random,
        None => {
            if page.cursors.len() >= 512 {
                return Err(GitFailure::Capacity);
            }
            let mut random = [0; 32];
            getrandom::fill(&mut random).map_err(|_| GitFailure::Unchecked)?;
            if random == [0; 32] || page.cursors.contains_key(&random) {
                return Err(GitFailure::Unchecked);
            }
            if let Some(residency) = &mut page.residency {
                residency.grow(128)?;
            }
            page.cursors.insert(random, offset);
            random
        }
    };
    GitPageCursor::new(GitPageCursorParts {
        scope: page.scope,
        epoch,
        observation: page.observation,
        random,
    })
}
fn produce_page(
    page: &mut PageObservation,
    offset: usize,
    epoch: spaceterm_core::authority::ServerEpoch,
) -> Result<GitQueryResult, GitFailure> {
    // Conservative encoded-byte costs include IDs, metadata and the next cursor.
    let mut bytes = 1024;
    let (mut result, end, length) = match &page.data {
        PageData::Inventory {
            revision, records, ..
        } => {
            let mut result = Vec::new();
            let mut end = offset;
            for record in records.iter().skip(offset) {
                let cost = 128 + record.directory.bytes().len();
                if result.len() >= MAX_GIT_PAGE_RECORDS || bytes + cost > MAX_GIT_PAGE_BYTES {
                    break;
                }
                bytes += cost;
                result.push(record.clone());
                end += 1;
            }
            (
                GitQueryResult::Inventory(GitInventoryPage {
                    records: result,
                    next: None,
                    version: *revision,
                }),
                end,
                records.len(),
            )
        }
        PageData::Branches { records, .. } => {
            let mut result = Vec::new();
            let mut end = offset;
            for record in records.iter().skip(offset) {
                let cost = 128 + record.name.as_str().len();
                if result.len() >= MAX_GIT_PAGE_RECORDS || bytes + cost > MAX_GIT_PAGE_BYTES {
                    break;
                }
                bytes += cost;
                result.push(record.clone());
                end += 1;
            }
            (
                GitQueryResult::Branches(GitBranchPage {
                    records: result,
                    next: None,
                }),
                end,
                records.len(),
            )
        }
        PageData::Assessment { token, plan, .. } => {
            let mut result = GitAssessmentPage {
                token: token.clone(),
                tabs: vec![],
                panes: vec![],
                fallbacks: vec![],
                next: None,
                requires_confirmation: true,
                creation_preview: None,
                removal: None,
            };
            match &plan.action {
                ConfirmedGitAction::Create { preview, .. } => {
                    result.creation_preview = Some(preview.clone());
                    (GitQueryResult::Assessment(Box::new(result)), 0, 0)
                }
                ConfirmedGitAction::Remove(scope) => {
                    result.removal = Some(scope.summary());
                    bytes += 128
                        + scope.facts().target.directory.bytes().len()
                        + result
                            .removal
                            .as_ref()
                            .and_then(|r| r.branch.as_ref())
                            .map_or(0, |b| b.as_str().len());
                    let parts = scope.retained().parts();
                    let length = parts.tabs.len() + parts.panes.len() + parts.fallbacks.len();
                    let mut end = offset;
                    while end < length && end - offset < MAX_GIT_PAGE_RECORDS {
                        let cost = if end < parts.tabs.len() + parts.panes.len() {
                            192
                        } else {
                            128 + parts.fallbacks[end - parts.tabs.len() - parts.panes.len()]
                                .directory
                                .bytes()
                                .len()
                        };
                        if bytes + cost > MAX_GIT_PAGE_BYTES {
                            break;
                        }
                        bytes += cost;
                        if end < parts.tabs.len() {
                            let tab = &parts.tabs[end];
                            let version = parts
                                .membership_versions
                                .iter()
                                .find(|v| v.id() == tab.id)
                                .ok_or(GitFailure::Unchecked)?
                                .version();
                            result.tabs.push(GitAffectedTab {
                                workspace: tab.workspace,
                                tab: tab.id,
                                worktree: tab.worktree,
                                membership_version: version,
                            });
                        } else if end < parts.tabs.len() + parts.panes.len() {
                            let pane = &parts.panes[end - parts.tabs.len()];
                            result.panes.push(GitAffectedPane {
                                pane: pane.hierarchy.pane,
                                tab: pane.hierarchy.tab,
                                session_generation: pane.hierarchy.session_generation,
                                control_generation: pane.hierarchy.control_generation,
                                work_generation: pane.work_generation,
                                starting: pane.startup_reserved,
                            });
                        } else {
                            let fallback =
                                &parts.fallbacks[end - parts.tabs.len() - parts.panes.len()];
                            result.fallbacks.push(GitFallback {
                                workspace: fallback.workspace,
                                worktree: Some(fallback.worktree),
                                directory: fallback.directory.clone(),
                            });
                        }
                        end += 1;
                    }
                    (GitQueryResult::Assessment(Box::new(result)), end, length)
                }
            }
        }
    };
    if end == offset && offset < length {
        return Err(GitFailure::Capacity);
    }
    if end < length {
        let cursor = next_cursor(page, end, epoch)?;
        match &mut result {
            GitQueryResult::Inventory(p) => p.next = Some(cursor),
            GitQueryResult::Branches(p) => p.next = Some(cursor),
            GitQueryResult::Assessment(p) => p.next = Some(cursor),
            _ => {}
        }
    }
    Ok(result)
}

fn validate_inventory(
    basis: &GitAuthoritySnapshot,
    inventory: &HostGitInventory,
) -> Result<(), GitFailure> {
    let server = basis.organization.state().server;
    let mut directories = std::collections::BTreeSet::new();
    let mut retained = std::collections::BTreeSet::new();
    if inventory.source.server() != server
        || inventory.repository.is_some_and(|id| id.server() != server)
    {
        return Err(GitFailure::WrongServer);
    }
    if inventory
        .current
        .is_some_and(|index| index >= inventory.records.len())
    {
        return Err(GitFailure::InvalidResponse);
    }
    if inventory.repository.is_none()
        && basis.repositories.iter().any(|r| {
            r.parts().main_root == inventory.main_root
                || r.parts().common_directory == inventory.common_directory
        })
    {
        return Err(GitFailure::StaleAssessment);
    }
    if let Some(repository) = inventory.repository {
        let locator = locator(basis, RepositoryScope { server, repository })?;
        if locator.parts().main_root != inventory.main_root
            || locator.parts().common_directory != inventory.common_directory
        {
            return Err(GitFailure::IdentityReplaced);
        }
    }
    for (index, record) in inventory.records.iter().enumerate() {
        if record.directory.server() != server
            || record.retained.is_some_and(|id| id.server() != server)
        {
            return Err(GitFailure::WrongServer);
        }
        if !directories.insert(record.directory.bytes()) || record.main != (index == 0) {
            return Err(GitFailure::InvalidResponse);
        }
        if let Some(id) = record.retained {
            if !retained.insert(id) {
                return Err(GitFailure::InvalidResponse);
            }
            let old = basis
                .organization
                .state()
                .worktrees
                .iter()
                .find(|w| w.id == id)
                .ok_or(GitFailure::IdentityReplaced)?;
            if Some(old.repository) != inventory.repository
                || old.directory != record.directory
                || old.main != record.main
            {
                return Err(GitFailure::IdentityReplaced);
            }
        } else if basis
            .organization
            .state()
            .worktrees
            .iter()
            .any(|w| Some(w.repository) == inventory.repository && w.directory == record.directory)
        {
            return Err(GitFailure::IdentityReplaced);
        }
    }
    Ok(())
}

fn validate_inventory_basis(
    captured: &GitAuthoritySnapshot,
    current: &GitAuthoritySnapshot,
    inventory: &HostGitInventory,
) -> Result<(), GitFailure> {
    if let Some(repository) = inventory.repository {
        let scope = RepositoryScope {
            server: repository.server(),
            repository,
        };
        if locator(captured, scope)? != locator(current, scope)?
            || inventory_version(captured, repository) != inventory_version(current, repository)
        {
            return Err(GitFailure::StaleAssessment);
        }
    } else if captured.repositories.len() != current.repositories.len()
        || captured
            .repositories
            .iter()
            .any(|r| !current.repositories.contains(r))
    {
        // An unbound discovery cannot rule out a newly registered physical alias.
        // Retry against the committed registry instead of allocating another identity.
        return Err(GitFailure::StaleAssessment);
    }
    Ok(())
}
