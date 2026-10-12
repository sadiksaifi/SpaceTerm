//! Replaceable Git read projections. Shared membership still comes only from
//! the organization replica. Incomplete or failed inventories never erase it.

use super::rpc::{ClientGitFailure, GitReadResult};
use spaceterm_core::{
    authority::{OrganizationRevision, ServerEpoch},
    identity::{WorkspaceId, WorktreeId},
    operations::RepositoryScope,
    worktrees::{queries::*, *},
};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitFreshness {
    Current,
    LastKnown,
    ResyncRequired,
}
#[derive(Clone, Debug)]
pub struct GitStatusReplica {
    target: GitStatusTarget,
    epoch: ServerEpoch,
    scope: Option<RepositoryScope>,
    facts: Option<GitStatusFacts>,
    freshness: GitFreshness,
}
impl GitStatusReplica {
    pub fn new(target: GitStatusTarget, epoch: ServerEpoch) -> Self {
        Self {
            target,
            epoch,
            scope: None,
            facts: None,
            freshness: GitFreshness::ResyncRequired,
        }
    }
    pub fn accept(&mut self, read: GitReadResult) -> Result<(), ClientGitFailure> {
        if read.epoch != self.epoch {
            return Err(ClientGitFailure::StaleResponse);
        }
        validate_read(&read, self.target.server())?;
        let GitQueryResult::Status(facts) = read.result else {
            return Err(ClientGitFailure::InvalidResponse);
        };
        if self
            .facts
            .as_ref()
            .is_some_and(|old| facts.revision <= old.revision)
        {
            return Err(ClientGitFailure::StaleResponse);
        }
        self.scope = read.scope;
        self.facts = Some(facts);
        self.freshness = GitFreshness::Current;
        Ok(())
    }
    pub fn mark_last_known(&mut self) {
        self.freshness = GitFreshness::LastKnown;
    }
    pub fn require_resync(&mut self) {
        self.freshness = GitFreshness::ResyncRequired;
    }
    pub fn facts(&self) -> Option<&GitStatusFacts> {
        self.facts.as_ref()
    }
    pub fn scope(&self) -> Option<RepositoryScope> {
        self.scope
    }
    pub fn freshness(&self) -> GitFreshness {
        self.freshness
    }
}

/// Local refresh ticket. It is not a protocol identity or an operation permit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InventoryTicket {
    generation: u64,
    epoch: ServerEpoch,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InventoryProgress {
    More(GitPageCursor),
    Complete,
}
struct StagingInventory {
    ticket: InventoryTicket,
    scope: Option<RepositoryScope>,
    version: Option<OrganizationRevision>,
    next: Option<GitPageCursor>,
    records: Vec<GitInventoryRecord>,
    ids: BTreeSet<WorktreeId>,
    cursors: BTreeSet<[u8; 32]>,
    bytes: usize,
}
pub struct GitInventoryReplica {
    workspace: WorkspaceId,
    generation: u64,
    epoch: Option<ServerEpoch>,
    version: Option<OrganizationRevision>,
    scope: Option<RepositoryScope>,
    records: Vec<GitInventoryRecord>,
    bytes: usize,
    // Retained separately from the last complete inventory so a refresh cannot
    // erase a newer authoritative organization observation.
    authoritative_basis: Option<(ServerEpoch, OrganizationRevision)>,
    staging: Option<StagingInventory>,
    needs_resync: bool,
}
impl GitInventoryReplica {
    pub fn new(workspace: WorkspaceId) -> Self {
        Self {
            workspace,
            generation: 0,
            epoch: None,
            version: None,
            scope: None,
            records: Vec::new(),
            bytes: 0,
            authoritative_basis: None,
            staging: None,
            needs_resync: true,
        }
    }
    pub fn begin(&mut self, epoch: ServerEpoch) -> Result<InventoryTicket, ClientGitFailure> {
        if epoch.get() == 0 {
            return Err(ClientGitFailure::InvalidResponse);
        }
        if self
            .authoritative_basis
            .is_some_and(|(known, _)| epoch < known)
        {
            return Err(ClientGitFailure::StaleResponse);
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(ClientGitFailure::Capacity)?;
        let ticket = InventoryTicket {
            generation: self.generation,
            epoch,
        };
        if self
            .authoritative_basis
            .is_none_or(|(known, _)| epoch > known)
        {
            self.authoritative_basis = Some((epoch, OrganizationRevision::new(0)));
        }
        self.staging = Some(StagingInventory {
            ticket,
            scope: None,
            version: None,
            next: None,
            records: Vec::new(),
            ids: BTreeSet::new(),
            cursors: BTreeSet::new(),
            bytes: 0,
        });
        self.needs_resync = true;
        Ok(ticket)
    }
    pub fn accept(
        &mut self,
        ticket: InventoryTicket,
        query: &GitQuery,
        read: GitReadResult,
    ) -> Result<InventoryProgress, ClientGitFailure> {
        if self.staging.as_ref().is_none_or(|s| s.ticket != ticket) {
            return Err(ClientGitFailure::StaleResponse);
        }
        let result = self.accept_checked(ticket, query, read);
        if result.is_err() {
            self.staging = None;
            self.needs_resync = true;
        }
        result
    }
    fn accept_checked(
        &mut self,
        ticket: InventoryTicket,
        query: &GitQuery,
        read: GitReadResult,
    ) -> Result<InventoryProgress, ClientGitFailure> {
        let GitQuery::Inventory { workspace, cursor } = query else {
            return Err(ClientGitFailure::InvalidResponse);
        };
        if *workspace != self.workspace {
            return Err(ClientGitFailure::WrongServer);
        }
        if read.epoch != ticket.epoch {
            return Err(ClientGitFailure::StaleResponse);
        }
        validate_read(&read, self.workspace.server())?;
        let GitQueryResult::Inventory(page) = read.result else {
            return Err(ClientGitFailure::InvalidResponse);
        };
        let s = self
            .staging
            .as_mut()
            .ok_or(ClientGitFailure::StaleResponse)?;
        if &s.next != cursor {
            return Err(ClientGitFailure::ResyncRequired);
        }
        if s.version.is_some_and(|v| v != page.version)
            || (s.version.is_some() && s.scope != read.scope)
        {
            return Err(ClientGitFailure::ResyncRequired);
        }
        if self
            .authoritative_basis
            .is_some_and(|(epoch, minimum)| ticket.epoch != epoch || page.version < minimum)
        {
            return Err(ClientGitFailure::ResyncRequired);
        }
        if page
            .next
            .as_ref()
            .is_some_and(|c| c.parts().epoch != ticket.epoch)
        {
            return Err(ClientGitFailure::ResyncRequired);
        }
        if s.records.len() + page.records.len() > MAX_GIT_REPOSITORIES {
            return Err(ClientGitFailure::Capacity);
        }
        let added = page
            .records
            .iter()
            .map(|r| 128 + r.directory.bytes().len())
            .sum::<usize>();
        if self.bytes + s.bytes + added > MAX_GIT_SCOPE_BYTES {
            return Err(ClientGitFailure::Capacity);
        }
        for record in &page.records {
            if !s.ids.insert(record.worktree) {
                return Err(ClientGitFailure::ResyncRequired);
            }
        }
        if let Some(next) = &page.next
            && (page.records.is_empty() || !s.cursors.insert(next.parts().random))
        {
            return Err(ClientGitFailure::ResyncRequired);
        }
        s.scope = read.scope;
        s.version = Some(page.version);
        s.bytes += added;
        s.records.extend(page.records);
        s.next = page.next;
        if let Some(next) = &s.next {
            return Ok(InventoryProgress::More(next.clone()));
        }
        let complete = self.staging.take().ok_or(ClientGitFailure::StaleResponse)?;
        self.epoch = Some(ticket.epoch);
        self.version = complete.version;
        self.scope = complete.scope;
        self.records = complete.records;
        self.bytes = complete.bytes;
        self.authoritative_basis = Some((ticket.epoch, page.version));
        self.needs_resync = false;
        Ok(InventoryProgress::Complete)
    }
    /// An organization delta is not a Git inventory patch. Invalidate the read
    /// projection and let the canonical organization replica apply membership.
    pub fn organization_changed(
        &mut self,
        epoch: ServerEpoch,
        _base: OrganizationRevision,
        revision: OrganizationRevision,
    ) {
        if self
            .authoritative_basis
            .is_some_and(|(known_epoch, minimum)| {
                epoch < known_epoch || (epoch == known_epoch && revision <= minimum)
            })
        {
            return;
        }
        self.authoritative_basis = Some((epoch, revision));
        if self.epoch != Some(epoch) || self.version.is_none_or(|v| revision > v) {
            self.require_resync();
        }
    }
    pub fn require_resync(&mut self) {
        self.staging = None;
        self.needs_resync = true;
    }
    pub fn records(&self) -> &[GitInventoryRecord] {
        &self.records
    }
    pub fn version(&self) -> Option<OrganizationRevision> {
        self.version
    }
    pub fn scope(&self) -> Option<RepositoryScope> {
        self.scope
    }
    pub fn needs_resync(&self) -> bool {
        self.needs_resync
    }
}
fn validate_read(
    read: &GitReadResult,
    server: spaceterm_core::identity::ServerId,
) -> Result<(), ClientGitFailure> {
    read.result
        .validate(read.scope, server)
        .map_err(ClientGitFailure::Remote)
}
