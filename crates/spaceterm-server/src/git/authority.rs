//! Git's typed bridge to the one organization actor. This module owns no store or allocator.
use crate::contracts::ConnectionAccessSnapshot;
use spaceterm_core::authority::{ServerEpoch, SessionKey};
use spaceterm_core::identity::{ClientId, PairingId, ServerId};
use spaceterm_core::operations::{OperationBinding, OperationId};
use spaceterm_core::storage::{CommitDisposition, CommitReceipt};
use spaceterm_core::worktrees::assessment::WorktreeRemovalScope;
use spaceterm_core::worktrees::*;
use std::future::Future;
use std::pin::Pin;

pub type GitFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
/// Constructed by the authenticated connection adapter, then checked again by the actor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthenticatedGitPrincipal {
    server: ServerId,
    epoch: ServerEpoch,
    pairing: PairingId,
    client: ClientId,
}
impl AuthenticatedGitPrincipal {
    pub fn from_connection_access(access: &ConnectionAccessSnapshot) -> Result<Self, GitFailure> {
        if !access.effects_enabled || access.epoch.get() == 0 {
            return Err(GitFailure::Unauthorized);
        }
        if access.pairing.server() != access.server || access.client.server() != access.server {
            return Err(GitFailure::WrongServer);
        }
        Ok(Self {
            server: access.server,
            epoch: access.epoch,
            pairing: access.pairing,
            client: access.client,
        })
    }
    pub fn server(self) -> ServerId {
        self.server
    }
    pub fn epoch(self) -> ServerEpoch {
        self.epoch
    }
    pub fn pairing(self) -> PairingId {
        self.pairing
    }
    pub fn client(self) -> ClientId {
        self.client
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfirmedGitAction {
    Create {
        preview: WorktreeCreationPreview,
        target: CreationTargetObservation,
    },
    Remove(Box<WorktreeRemovalScope>),
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfirmedGitPlan {
    pub repository: RepositoryLocator,
    pub action: ConfirmedGitAction,
    pub assessment_digest: GitAssessmentDigest,
}
impl ConfirmedGitPlan {
    pub fn kind(&self) -> GitAssessmentKind {
        match self.action {
            ConfirmedGitAction::Create { .. } => GitAssessmentKind::Create,
            ConfirmedGitAction::Remove(_) => GitAssessmentKind::Remove,
        }
    }
    pub fn validate_scope(&self) -> Result<(), GitFailure> {
        match &self.action {
            ConfirmedGitAction::Create { preview, .. }
                if preview.parts().draft.parts().repository != self.repository.scope() =>
            {
                Err(GitFailure::WrongServer)
            }
            ConfirmedGitAction::Remove(scope)
                if scope.facts().target.repository != self.repository.scope().repository =>
            {
                Err(GitFailure::WrongServer)
            }
            _ => Ok(()),
        }
    }
    pub fn validate_intent(&self, intent: &GitWriteIntent) -> Result<(), GitFailure> {
        self.validate_scope()?;
        if intent.parts().repository != self.repository
            || intent.parts().assessment_digest != self.assessment_digest
        {
            return Err(GitFailure::StaleAssessment);
        }
        let valid = match (&self.action, &intent.parts().action) {
            (
                ConfirmedGitAction::Create { preview, target },
                RetainedGitAction::Create {
                    directory,
                    branch,
                    target: actual,
                    template_version,
                    ..
                },
            ) => {
                preview.parts().draft.parts().repository == self.repository.scope()
                    && directory == &preview.parts().directory
                    && branch == &preview.parts().draft.parts().branch
                    && actual == target
                    && template_version == &preview.parts().template_version
            }
            (
                ConfirmedGitAction::Remove(scope),
                RetainedGitAction::Remove {
                    worktree,
                    directory,
                    head,
                    approval,
                },
            ) => {
                *worktree == scope.facts().target.id
                    && directory == &scope.facts().target.directory
                    && head == &scope.facts().head
                    && *approval == scope.approval()
                    && intent.parts().affected == *scope.retained()
            }
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(GitFailure::StaleAssessment)
        }
    }
}
#[derive(Clone, Debug)]
pub struct ConfirmedGitAdmission {
    pub principal: AuthenticatedGitPrincipal,
    pub binding: OperationBinding,
    pub command: GitCommand,
    pub plan: ConfirmedGitPlan,
}
/// Non-serializable actor lease. Its IDs were allocated once, after durable ledger admission.
#[derive(Debug)]
pub struct GitReservation {
    intent: GitWriteIntent,
    allocation: GitAllocationReservation,
}
impl GitReservation {
    pub fn new(
        intent: GitWriteIntent,
        allocation: GitAllocationReservation,
    ) -> Result<Self, GitFailure> {
        if intent.binding() != allocation.binding() {
            return Err(GitFailure::StaleAssessment);
        }
        match intent.parts().action {
            RetainedGitAction::Create { worktree, .. }
                if allocation.parts().worktree != Some(worktree) =>
            {
                return Err(GitFailure::InvalidResponse);
            }
            RetainedGitAction::Remove { .. } if allocation.parts().worktree.is_some() => {
                return Err(GitFailure::InvalidResponse);
            }
            _ => {}
        }
        let expected = &intent.parts().affected.parts().fallbacks;
        if expected.len() != allocation.parts().fallbacks.len()
            || expected
                .iter()
                .any(|p| !allocation.parts().fallbacks.iter().any(|r| &r.plan == p))
        {
            return Err(GitFailure::InvalidResponse);
        }
        Ok(Self { intent, allocation })
    }
    pub fn intent(&self) -> &GitWriteIntent {
        &self.intent
    }
    pub fn allocation(&self) -> &GitAllocationReservation {
        &self.allocation
    }
    pub fn binding(&self) -> OperationBinding {
        self.intent.binding()
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GitOperationLookup {
    New(OperationBinding),
    Pending(OperationBinding),
    Completed(GitMutationOutcome),
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitRetirementReport {
    /// Exact old owners whose cleanup was confirmed, including already-retiring owners.
    pub cleaned: Vec<SessionKey>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitCompletionPlan {
    pub intent: GitWriteIntent,
    pub external: RetainedExternalResult,
    /// Present only for observation-based restart settlement. Never proof of historical success.
    pub recovery: Option<RecoveryObservation>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitCompletionCommit {
    pub disposition: CommitDisposition,
    pub outcome: Option<GitMutationOutcome>,
}
impl GitCompletionCommit {
    pub fn durable(receipt: CommitReceipt, outcome: GitMutationOutcome) -> Self {
        Self {
            disposition: CommitDisposition::Durable(receipt),
            outcome: Some(outcome),
        }
    }
}

/// A single committed actor observation. Display reports remain presentation-only;
/// only the reducer's committed restoration field may supply removal authority.
#[derive(Clone, Debug)]
pub struct GitAuthoritySnapshot {
    pub organization: spaceterm_core::organization::OrganizationSnapshot,
    pub repositories: Vec<RepositoryLocator>,
    pub home: spaceterm_core::organization::ServerDirectory,
    pub display_directories: Vec<spaceterm_core::observations::DisplayDirectoryObservation>,
    pub runtime: spaceterm_core::worktrees::assessment::RuntimeWorkSet,
}

/// Inventory entries carry optional retained identities, never freshly allocated IDs.
/// Membership destinations are indices into the same complete observation.
#[derive(Clone, Debug)]
pub struct GitInventoryReconciliation {
    pub basis: GitAuthoritySnapshot,
    pub inventory: super::host::HostGitInventory,
    pub memberships: Vec<GitInventoryMembership>,
}
#[derive(Clone, Debug)]
pub struct GitOutsideSourceReconciliation {
    pub basis: GitAuthoritySnapshot,
    pub directory: spaceterm_core::organization::ServerDirectory,
    pub memberships: Vec<WorktreeMembershipEdit>,
}
pub use spaceterm_core::reducer::{GitInventoryMembership, WorktreeMembershipEdit};

/// All methods serialize through the actor; none holds an organization lock while awaiting IO.
pub trait GitOrganizationPort: Send + Sync {
    /// The frozen read bridge. Write-only compositions fail closed until supplied.
    fn snapshot(&self) -> GitFuture<'_, Result<GitAuthoritySnapshot, GitFailure>> {
        Box::pin(async { Err(GitFailure::StorageUnavailable) })
    }
    /// Revalidate the observation basis, ask the reducer to allocate any new IDs,
    /// and commit locator, inventory, memberships, Roots and revision together.
    /// Retain the inventory's native lease at the durable commit, even if the read waiter
    /// has gone away. Publish through the actor's organization hub to every authorized connection,
    /// not merely the querying principal or attached/active Workspaces. Reject
    /// conflicting Git/destructive reservations; a Started write's uncommitted
    /// external state cannot register as a different new Worktree.
    fn reconcile_inventory(
        &self,
        _principal: AuthenticatedGitPrincipal,
        _request: GitInventoryReconciliation,
    ) -> GitFuture<'_, Result<GitAuthoritySnapshot, GitFailure>> {
        Box::pin(async { Err(GitFailure::StorageUnavailable) })
    }
    /// A checked source with no visible working tree may clear unpinned Root membership only.
    /// Compare source/Session and canonical membership versions against the basis, reject
    /// conflicting reservations, and publish the committed change to every authorized client.
    /// Keep all retained repository and Worktree records. Failed probes never call this port.
    fn reconcile_outside_source(
        &self,
        _principal: AuthenticatedGitPrincipal,
        _request: GitOutsideSourceReconciliation,
    ) -> GitFuture<'_, Result<GitAuthoritySnapshot, GitFailure>> {
        Box::pin(async { Err(GitFailure::StorageUnavailable) })
    }
    fn authorize(
        &self,
        principal: AuthenticatedGitPrincipal,
    ) -> GitFuture<'_, Result<(), GitFailure>>;
    /// The actor uses the real canonical LedgerCodec. Completed lookup precedes token inspection.
    fn lookup<'a>(
        &'a self,
        principal: AuthenticatedGitPrincipal,
        operation: OperationId,
        command: &'a GitCommand,
    ) -> GitFuture<'a, Result<GitOperationLookup, GitFailure>>;
    /// One durable store-only ledger admission, quotas and operation-bound allocations.
    fn reserve_confirmed(
        &self,
        request: ConfirmedGitAdmission,
    ) -> GitFuture<'_, Result<GitReservation, GitFailure>>;
    /// Advance the existing pending ledger identity; insert Prepared and its allocation atomically.
    fn persist_prepared<'a>(
        &'a self,
        reservation: &'a GitReservation,
    ) -> GitFuture<'a, CommitDisposition>;
    /// Install the exact input/startup barrier before comparing work stamps again.
    fn barrier_and_revalidate<'a>(
        &'a self,
        reservation: &'a GitReservation,
    ) -> GitFuture<'a, Result<(), GitFailure>>;
    fn persist_started<'a>(
        &'a self,
        reservation: &'a GitReservation,
    ) -> GitFuture<'a, CommitDisposition>;
    /// Use the reducer's non-deleting Worktree retirement; join each first-reason owner once.
    fn retire_affected<'a>(
        &'a self,
        reservation: &'a GitReservation,
    ) -> GitFuture<'a, Result<GitRetirementReport, GitFailure>>;
    /// Service-owned settlement, not a newly authorized mutation. Recheck current hierarchy and
    /// unresolved destructive gates. Never resurrect deleted targets. Stop/End All/revocation
    /// cannot reject indispensable Started settlement or launch fallback shells. Consume only
    /// the original reserved IDs; release a revoked creator's new control in this same commit.
    /// Commit membership, intent removal, ledger result and operation-owned revision together.
    fn complete<'a>(
        &'a self,
        reservation: &'a GitReservation,
        completion: &'a GitCompletionPlan,
    ) -> GitFuture<'a, GitCompletionCommit>;
    /// Release logical gates only after a confirmed final commit, including durable rejection.
    fn release(&self, reservation: GitReservation) -> GitFuture<'_, ()>;
    /// Restore original gates and require trusted previous-instance process cleanup evidence.
    /// A retired initiating stream remains eligible for Started settlement, not execution.
    fn recover_reservation<'a>(
        &'a self,
        intent: &'a GitWriteIntent,
    ) -> GitFuture<'a, Result<GitReservation, GitFailure>>;
    fn close_mutation_admission(&self) -> GitFuture<'_, ()>;
}
