//! Client-local Pull Request lookup from normalized owning-server Git facts.
//!
//! Desktop owners feed status and visibility changes, run returned tickets off the UI thread,
//! and return each completion to this transient cache. No Git discovery or persistence runs here.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use spaceterm_core::identity::RepositoryId;
use spaceterm_core::organization::BranchName;
use spaceterm_core::worktrees::GitStatusTarget;
use spaceterm_core::worktrees::queries::{GitPullRequestFacts, GitStatusFacts, GitUpstream};

use super::pull_request::PullRequestQuery;
use super::pull_request_lookup::{GitHubCliLookup, GitHubHosts};
use super::{
    GitHubCliStatus, GitHubRepository, PullRequest, PullRequestError, RepositoryStatusPreferences,
};
use crate::ssh::cancellation::SshCancellationToken;

const MAX_INTERESTS: usize = 256;
const MAX_CACHED_BRANCHES: usize = 256;
const MAX_IN_FLIGHT: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum ServerPullRequestFailure {
    #[error("client Pull Request lookup capacity is exhausted")]
    Capacity,
    #[error("Pull Request status target and repository belong to different servers")]
    WrongServer,
}

#[derive(Clone, Eq, PartialEq)]
struct BranchKey {
    repository: RepositoryId,
    branch: BranchName,
    // Worktree-local configuration can select different queries for the same local branch.
    query: PullRequestQuery,
}

/// A single client-local request. Return it unchanged even after cancellation, so its worker slot
/// can be released. Cloning a ticket does not authorize a second execution.
#[derive(Clone)]
pub(crate) struct ServerPullRequestTicket(Arc<LookupRequest>);

struct LookupRequest {
    key: BranchKey,
    generation: u64,
    cancellation: SshCancellationToken,
    started: Instant,
}

impl fmt::Debug for ServerPullRequestTicket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ServerPullRequestTicket")
            .field("generation", &self.0.generation)
            .finish_non_exhaustive()
    }
}

impl ServerPullRequestTicket {
    pub(crate) fn repository(&self) -> RepositoryId {
        self.0.key.repository
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.cancellation.is_cancelled()
    }

    /// Blocking. Uses only the existing client GitHub CLI provider and its client-home execution
    /// policy. Enterprise hosts must pass the existing authentication check before lookup.
    pub(crate) fn run(
        &self,
        lookup: Option<&GitHubCliLookup>,
        hosts: &GitHubHosts,
    ) -> Result<Option<PullRequest>, PullRequestError> {
        if self.is_cancelled() {
            return Err(PullRequestError::Cancelled);
        }
        let lookup = lookup.ok_or(PullRequestError::ToolMissing)?;
        if !hosts.is_github_host(
            lookup,
            &self.0.key.query.repository.host,
            &self.0.cancellation,
        )? {
            return Ok(None);
        }
        if self.is_cancelled() {
            return Err(PullRequestError::Cancelled);
        }
        let result =
            lookup.find_pull_request(&self.0.key.query, Instant::now(), &self.0.cancellation);
        if self.is_cancelled() {
            Err(PullRequestError::Cancelled)
        } else {
            result
        }
    }
}

struct CurrentInterest {
    target: GitStatusTarget,
    key: Option<BranchKey>,
    upstream: Option<GitUpstream>,
}

struct CachedBranch {
    key: BranchKey,
    value: Option<PullRequest>,
    in_flight: Option<u64>,
}

/// App-scoped, transient Pull Request state. RepositoryId already includes Server identity.
///
/// Retains at most 256 status targets and 256 branch/query entries, evicting only inactive caches.
/// Each target selects its own Server/repository/branch/query binding. Targets share lookup work
/// only when that entire binding matches, including the normalized query. Owners call `remove`
/// after the last local subscriber leaves a target and `set_policy` for client policy changes.
/// At most four workers run, with one coalesced follow-up per binding. Cancellation keeps its slot
/// occupied until `finished` acknowledges completion, including worker-spawn failure.
pub(crate) struct ServerPullRequests {
    enabled: bool,
    current: Vec<CurrentInterest>,
    cache: VecDeque<CachedBranch>,
    ready: VecDeque<BranchKey>,
    in_flight: BTreeMap<u64, ServerPullRequestTicket>,
    generation: u64,
}

impl ServerPullRequests {
    pub(crate) fn new(
        preferences: RepositoryStatusPreferences,
        github_cli: GitHubCliStatus,
    ) -> Self {
        Self {
            enabled: preferences.pull_requests_enabled() && github_cli == GitHubCliStatus::SignedIn,
            current: Vec::new(),
            cache: VecDeque::new(),
            ready: VecDeque::new(),
            in_flight: BTreeMap::new(),
            generation: 0,
        }
    }

    /// Observe already-validated server status for the caller's qualified status target. Branch,
    /// upstream and normalized-query changes trigger lookup; revisions, change counts and repaint
    /// do not. The local branch and complete normalized query key the cache; the normalized head
    /// branch is the CLI operand.
    pub(crate) fn observe(
        &mut self,
        target: GitStatusTarget,
        repository: RepositoryId,
        facts: &GitStatusFacts,
        now: Instant,
    ) -> Result<Vec<ServerPullRequestTicket>, ServerPullRequestFailure> {
        if target.server() != repository.server() {
            return Err(ServerPullRequestFailure::WrongServer);
        }
        let position = self
            .current
            .iter()
            .position(|interest| interest.target == target);
        if position.is_none() && self.current.len() == MAX_INTERESTS {
            return Err(ServerPullRequestFailure::Capacity);
        }
        let key = facts
            .branch
            .as_ref()
            .zip(facts.pull_request.as_ref())
            .map(|(branch, query)| BranchKey {
                repository,
                branch: branch.clone(),
                query: normalized_query(query),
            });
        let changed = position.is_none_or(|position| {
            let previous = &self.current[position];
            previous.key != key || previous.upstream != facts.upstream
        });
        let interest = CurrentInterest {
            target,
            key: key.clone(),
            upstream: facts.upstream.clone(),
        };
        if let Some(position) = position {
            self.current[position] = interest;
        } else {
            self.current.push(interest);
        }
        // Only the last target leaving a branch/query binding cancels its work. Completed inactive
        // values remain cached, but cancelled generations cannot refresh a rebound target.
        self.cancel_inactive();
        if let Some(key) = key {
            let position = self.cache.iter().position(|entry| entry.key == key);
            let entry = if let Some(position) = position {
                // `position` comes from this same deque, with no intervening mutation.
                self.cache
                    .remove(position)
                    .ok_or(ServerPullRequestFailure::Capacity)?
            } else {
                if self.cache.len() == MAX_CACHED_BRANCHES {
                    let inactive = self
                        .cache
                        .iter()
                        .position(|entry| !self.is_current(&entry.key))
                        .ok_or(ServerPullRequestFailure::Capacity)?;
                    self.cache.remove(inactive);
                }
                CachedBranch {
                    key,
                    value: None,
                    in_flight: None,
                }
            };
            if self.enabled && (changed || position.is_none()) {
                request(&entry, &self.in_flight, &mut self.ready, now);
            }
            self.cache.push_back(entry);
        } else {
            // Detached HEAD hides cached branches without discarding them. Missing query facts
            // invalidate only the named branch; other completed branch values remain reusable.
            self.cache.retain_mut(|entry| {
                if entry.key.repository != repository
                    || facts.branch.as_ref() != Some(&entry.key.branch)
                    || self
                        .current
                        .iter()
                        .any(|interest| interest.key.as_ref() == Some(&entry.key))
                {
                    return true;
                }
                cancel(entry, &self.in_flight, &mut self.ready);
                false
            });
        }
        self.start_ready(now)
    }

    /// Refresh only status targets visible in the activated window. Matching branch/query bindings
    /// coalesce.
    pub(crate) fn window_activated(
        &mut self,
        visible: impl IntoIterator<Item = GitStatusTarget>,
        now: Instant,
    ) -> Result<Vec<ServerPullRequestTicket>, ServerPullRequestFailure> {
        if self.enabled {
            for target in visible {
                if let Some(key) = self
                    .current
                    .iter()
                    .find(|interest| interest.target == target)
                    .and_then(|interest| interest.key.as_ref())
                    && let Some(entry) = self.cache.iter().find(|entry| entry.key == *key)
                {
                    request(entry, &self.in_flight, &mut self.ready, now);
                }
            }
        }
        self.start_ready(now)
    }

    pub(crate) fn set_policy(
        &mut self,
        preferences: RepositoryStatusPreferences,
        github_cli: GitHubCliStatus,
        now: Instant,
    ) -> Result<Vec<ServerPullRequestTicket>, ServerPullRequestFailure> {
        let enabled =
            preferences.pull_requests_enabled() && github_cli == GitHubCliStatus::SignedIn;
        if !enabled {
            self.disable();
        } else if !self.enabled {
            self.enabled = true;
            for entry in &mut self.cache {
                if self
                    .current
                    .iter()
                    .any(|interest| interest.key.as_ref() == Some(&entry.key))
                {
                    request(entry, &self.in_flight, &mut self.ready, now);
                }
            }
        }
        self.start_ready(now)
    }

    /// Drop this status interest, cancelling work only when its branch/query binding has no other
    /// interests.
    /// Lookup results remain client-local; no organization, view-store or journal writes occur.
    pub(crate) fn remove(
        &mut self,
        target: GitStatusTarget,
        now: Instant,
    ) -> Result<Vec<ServerPullRequestTicket>, ServerPullRequestFailure> {
        self.current.retain(|interest| interest.target != target);
        self.cancel_inactive();
        self.start_ready(now)
    }

    /// Select the result for this target's current repository, branch and query, never a display name.
    pub(crate) fn view(&self, target: GitStatusTarget) -> Option<&PullRequest> {
        if !self.enabled {
            return None;
        }
        let key = self
            .current
            .iter()
            .find(|interest| interest.target == target)?
            .key
            .as_ref()?;
        self.cache
            .iter()
            .find(|entry| entry.key == *key)
            .and_then(|entry| entry.value.as_ref())
    }

    /// Ignore cancelled, duplicated or obsolete generations. Transient errors keep the last
    /// result; signed-out clears only this branch/query binding, while a missing tool clears every
    /// result.
    pub(crate) fn finished(
        &mut self,
        ticket: ServerPullRequestTicket,
        result: Result<Option<PullRequest>, PullRequestError>,
        now: Instant,
    ) -> Result<Vec<ServerPullRequestTicket>, ServerPullRequestFailure> {
        if !self
            .in_flight
            .get(&ticket.0.generation)
            .is_some_and(|running| Arc::ptr_eq(&running.0, &ticket.0))
        {
            return Ok(Vec::new());
        }
        self.in_flight.remove(&ticket.0.generation);
        if !ticket.is_cancelled()
            && let Some(entry) = self
                .cache
                .iter_mut()
                .find(|entry| entry.key == ticket.0.key)
            && entry.in_flight == Some(ticket.0.generation)
        {
            entry.in_flight = None;
            match result {
                Ok(value) => entry.value = value,
                Err(PullRequestError::NotLoggedIn) => entry.value = None,
                Err(PullRequestError::ToolMissing) => self.disable(),
                Err(
                    PullRequestError::Unavailable
                    | PullRequestError::InvalidResponse
                    | PullRequestError::Cancelled,
                ) => {}
            }
        }
        self.start_ready(now)
    }

    fn is_current(&self, key: &BranchKey) -> bool {
        self.current
            .iter()
            .any(|interest| interest.key.as_ref() == Some(key))
    }

    fn cancel_inactive(&mut self) {
        for entry in &mut self.cache {
            if !self
                .current
                .iter()
                .any(|interest| interest.key.as_ref() == Some(&entry.key))
            {
                cancel(entry, &self.in_flight, &mut self.ready);
            }
        }
    }

    fn disable(&mut self) {
        self.enabled = false;
        for entry in &mut self.cache {
            cancel(entry, &self.in_flight, &mut self.ready);
            entry.value = None;
        }
    }

    fn start_ready(
        &mut self,
        now: Instant,
    ) -> Result<Vec<ServerPullRequestTicket>, ServerPullRequestFailure> {
        let slots = MAX_IN_FLIGHT - self.in_flight.len();
        let ready = self
            .ready
            .iter()
            .filter(|key| {
                self.cache
                    .iter()
                    .any(|entry| entry.key == **key && entry.in_flight.is_none())
            })
            .count()
            .min(slots);
        self.generation
            .checked_add(ready as u64)
            .ok_or(ServerPullRequestFailure::Capacity)?;
        let mut tickets = Vec::with_capacity(ready);
        while tickets.len() < ready {
            let position = self
                .ready
                .iter()
                .position(|key| {
                    self.cache
                        .iter()
                        .any(|entry| entry.key == *key && entry.in_flight.is_none())
                })
                .ok_or(ServerPullRequestFailure::Capacity)?;
            let key = self
                .ready
                .remove(position)
                .ok_or(ServerPullRequestFailure::Capacity)?;
            let entry = self
                .cache
                .iter_mut()
                .find(|entry| entry.key == key)
                .ok_or(ServerPullRequestFailure::Capacity)?;
            self.generation += 1;
            entry.in_flight = Some(self.generation);
            let ticket = ServerPullRequestTicket(Arc::new(LookupRequest {
                key: entry.key.clone(),
                generation: self.generation,
                cancellation: SshCancellationToken::default(),
                started: now,
            }));
            self.in_flight.insert(self.generation, ticket.clone());
            tickets.push(ticket);
        }
        Ok(tickets)
    }
}

impl Drop for ServerPullRequests {
    fn drop(&mut self) {
        for running in self.in_flight.values() {
            running.0.cancellation.cancel();
        }
    }
}

fn request(
    entry: &CachedBranch,
    running: &BTreeMap<u64, ServerPullRequestTicket>,
    ready: &mut VecDeque<BranchKey>,
    now: Instant,
) {
    if !ready.contains(&entry.key)
        && entry
            .in_flight
            .and_then(|generation| running.get(&generation))
            .is_none_or(|flight| flight.0.started < now)
    {
        // Coalescing never changes an existing queue position. Cache recency is independent.
        ready.push_back(entry.key.clone());
    }
}

fn cancel(
    entry: &mut CachedBranch,
    running: &BTreeMap<u64, ServerPullRequestTicket>,
    ready: &mut VecDeque<BranchKey>,
) {
    if let Some(generation) = entry.in_flight.take()
        && let Some(flight) = running.get(&generation)
    {
        flight.0.cancellation.cancel();
    }
    ready.retain(|key| key != &entry.key);
}

fn normalized_query(facts: &GitPullRequestFacts) -> PullRequestQuery {
    PullRequestQuery {
        repository: GitHubRepository {
            host: Arc::from(facts.host()),
            owner: Arc::from(facts.owner()),
            name: Arc::from(facts.repository()),
        },
        head_branch: Arc::from(facts.head_branch().as_str()),
        head_owner: facts.head_owner().map(Arc::from),
    }
}

#[cfg(test)]
mod tests;
