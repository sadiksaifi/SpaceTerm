//! Blocking native Git bridge. It reuses hardened host execution and the existing serializer.
use super::authority::GitAuthoritySnapshot;
use super::authority::{ConfirmedGitAction, ConfirmedGitPlan};
use super::writes::DurableGitStartPermit;
use spaceterm_core::operations::OperationBinding;
use spaceterm_core::organization::{ServerDirectory, ServerSettingsRecord};
use spaceterm_core::server_directories::ServerDirectoryQueries;
use spaceterm_core::worktrees::queries::*;
use spaceterm_core::worktrees::*;
use spaceterm_host::git as native;
use spaceterm_host::git::filesystem::NativeWorktreeFilesystem;
use spaceterm_host::git::server::{
    ExecutedRepositoryWrite, FinalWriteValidation, PreparedRepositoryWrite,
};
use spaceterm_host::server_directories::native::NativeDirectoryProvider;
use spaceterm_host::server_directories::validation::CreateTargetLease;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

pub use native::CancellationToken as GitReadCancellation;
pub struct HostGitWriteRequest {
    pub intent: GitWriteIntent,
    pub native: Arc<dyn GitHostAssessment>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostWriteObservation {
    pub binding: OperationBinding,
    pub assessment_digest: GitAssessmentDigest,
}
pub trait GitHostAssessment: Send + Sync {
    fn prepare(
        &self,
        intent: &GitWriteIntent,
        cancellation: &GitReadCancellation,
    ) -> Result<Box<dyn PreparedServerGitWrite>, GitFailure>;
}
#[derive(Clone, Debug)]
pub struct HostGitInventory {
    /// The exact source Git discovered and its current entry, never containment inference.
    pub source: ServerDirectory,
    pub current: Option<usize>,
    pub lease: Option<Arc<HostInventoryLease>>,
    pub main_root: ServerDirectory,
    pub common_directory: ServerDirectory,
    pub repository: Option<spaceterm_core::identity::RepositoryId>,
    pub records: Vec<HostGitWorktree>,
}
#[derive(Clone, Debug)]
pub struct HostGitWorktree {
    pub retained: Option<spaceterm_core::identity::WorktreeId>,
    pub directory: ServerDirectory,
    pub main: bool,
    pub missing: bool,
    pub bare: bool,
    pub locked: bool,
    pub branch: Option<spaceterm_core::organization::BranchName>,
}
#[derive(Clone, Debug)]
pub struct HostGitSource {
    pub common_directory: ServerDirectory,
    pub worktree: ServerDirectory,
}
#[derive(Clone, Debug)]
pub struct HostGitStatus {
    pub inventory: Option<HostGitInventory>,
    pub facts: queries::GitStatusFacts,
}

pub trait ServerGitHost: Send + Sync {
    fn discover_source(
        &self,
        _directory: &ServerDirectory,
        _basis: &GitAuthoritySnapshot,
        _cancellation: &GitReadCancellation,
    ) -> Result<Option<HostGitSource>, GitFailure> {
        Err(GitFailure::Unchecked)
    }
    fn status(
        &self,
        _directory: &ServerDirectory,
        _basis: &super::authority::GitAuthoritySnapshot,
        _cancellation: &GitReadCancellation,
    ) -> Result<HostGitStatus, GitFailure> {
        Err(GitFailure::Unchecked)
    }
    fn inventory(
        &self,
        _directory: &ServerDirectory,
        _basis: &super::authority::GitAuthoritySnapshot,
        _cancellation: &GitReadCancellation,
    ) -> Result<Option<HostGitInventory>, GitFailure> {
        Err(GitFailure::Unchecked)
    }
    fn branches(
        &self,
        _repository: &RepositoryLocator,
        _cancellation: &GitReadCancellation,
    ) -> Result<Vec<queries::GitBranchRecord>, GitFailure> {
        Err(GitFailure::Unchecked)
    }
    fn creation_assessment(
        &self,
        _draft: WorktreeCreateDraft,
        _basis: &super::authority::GitAuthoritySnapshot,
        _cancellation: &GitReadCancellation,
    ) -> Result<(Arc<ConfirmedGitPlan>, Arc<dyn GitHostAssessment>), GitFailure> {
        Err(GitFailure::Unchecked)
    }
    fn removal_assessment(
        &self,
        _worktree: spaceterm_core::identity::WorktreeId,
        _basis: &super::authority::GitAuthoritySnapshot,
        _cancellation: &GitReadCancellation,
    ) -> Result<(Arc<ConfirmedGitPlan>, Arc<dyn GitHostAssessment>), GitFailure> {
        Err(GitFailure::Unchecked)
    }
    fn containment(
        &self,
        _directory: &ServerDirectory,
        _root: &ServerDirectory,
    ) -> spaceterm_core::server_directories::DirectoryContainment {
        spaceterm_core::server_directories::DirectoryContainment::Unchecked
    }
    fn prepare_write(
        &self,
        request: HostGitWriteRequest,
        cancellation: &GitReadCancellation,
    ) -> Result<Box<dyn PreparedServerGitWrite>, GitFailure>;
    fn reconcile(
        &self,
        intent: &GitWriteIntent,
        cancellation: &GitReadCancellation,
    ) -> Result<HostRecoveryObservation, GitFailure>;
    fn shutdown(&self) -> Result<(), GitFailure>;
}
pub trait PreparedServerGitWrite: Send {
    fn observation(&self) -> &HostWriteObservation;
    fn execute(self: Box<Self>, permit: DurableGitStartPermit) -> HostGitCompletion;
}
pub trait GitHostSettlement: Send {
    fn finish(self: Box<Self>, recorded: bool) -> Result<(), GitFailure>;
}
pub struct HostGitCompletion {
    pub result: RetainedExternalResult,
    pub settlement: Option<Box<dyn GitHostSettlement>>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostRecoveryObservation {
    pub observation: RecoveryObservation,
    pub external: RetainedExternalResult,
}

type DirectoryIdentity = Arc<dyn native::filesystem::RetainedDirectory>;
#[derive(Default)]
struct InventoryIdentities {
    entries: BTreeMap<Vec<u8>, InventoryIdentity>,
}
struct InventoryIdentity {
    committed: Option<DirectoryIdentity>,
    observed: Weak<dyn native::filesystem::RetainedDirectory>,
}
/// Native identity proof owned by one complete observation. Uncommitted leases
/// disappear with the observation; only actor-committed records retain them.
pub struct HostInventoryLease {
    registry: Arc<Mutex<InventoryIdentities>>,
    identities: Vec<(Vec<u8>, DirectoryIdentity)>,
}
impl std::fmt::Debug for HostInventoryLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HostInventoryLease(<redacted>)")
    }
}
impl HostInventoryLease {
    fn retain(
        &mut self,
        provider: &NativeDirectoryProvider,
        directory: &ServerDirectory,
        committed: bool,
    ) -> Result<(), GitFailure> {
        if self
            .identities
            .iter()
            .any(|(key, _)| key == directory.bytes())
        {
            return Ok(());
        }
        let mut registry = self.registry.lock().map_err(|_| GitFailure::Unchecked)?;
        registry
            .entries
            .retain(|_, entry| entry.committed.is_some() || entry.observed.strong_count() != 0);
        let identity = match registry
            .entries
            .get(directory.bytes())
            .and_then(|entry| entry.observed.upgrade())
        {
            Some(identity) => {
                identity
                    .revalidate()
                    .map_err(|_| GitFailure::IdentityReplaced)?;
                identity
            }
            None => {
                // Keep native-handle headroom for confirmations and final write validation.
                if registry.entries.len() >= 32 {
                    return Err(GitFailure::Capacity);
                }
                provider.retain(directory).map_err(directory_failure)?
            }
        };
        let entry = registry
            .entries
            .entry(directory.bytes().to_vec())
            .or_insert_with(|| InventoryIdentity {
                committed: None,
                observed: Arc::downgrade(&identity),
            });
        if committed {
            entry.committed = Some(identity.clone());
        }
        self.identities.push((directory.bytes().to_vec(), identity));
        Ok(())
    }
    pub fn revalidate(&self) -> Result<(), GitFailure> {
        for (_, identity) in &self.identities {
            identity
                .revalidate()
                .map_err(|_| GitFailure::IdentityReplaced)?;
        }
        Ok(())
    }
    /// The actor retains these identities at its durable inventory commit, before publication.
    /// This is idempotent so the service may also confirm a returned committed snapshot.
    pub fn retain_committed(&self) -> Result<(), GitFailure> {
        let mut registry = self.registry.lock().map_err(|_| GitFailure::Unchecked)?;
        for (key, identity) in &self.identities {
            if let Some(entry) = registry.entries.get_mut(key) {
                entry.committed = Some(identity.clone());
            }
        }
        Ok(())
    }
}

pub struct NativeServerGitHost {
    coordinator: native::GitCoordinator,
    worktrees: Arc<native::LocalWorktreeGit>,
    directories: Arc<NativeDirectoryProvider>,
    head_reader: Arc<HeadReader>,
    inventory_identities: Arc<Mutex<InventoryIdentities>>,
}
impl NativeServerGitHost {
    /// Production account composition runs hardened Git directly. Native spawning
    /// retains the descriptor guard and isolated process-group cleanup; no
    /// privileged spawner or credential receiver participates.
    pub fn for_account(
        git: PathBuf,
        home: PathBuf,
        version: native::ToolVersion,
        directories: Arc<NativeDirectoryProvider>,
    ) -> Self {
        Self::new(
            Arc::new(native::UnixRepositoryProgramRunner::new()),
            git,
            home,
            version,
            directories,
        )
    }

    pub fn new(
        runner: Arc<dyn native::RepositoryProgramRunner>,
        git: PathBuf,
        home: PathBuf,
        version: native::ToolVersion,
        directories: Arc<NativeDirectoryProvider>,
    ) -> Self {
        let worktrees = Arc::new(native::LocalWorktreeGit::new(
            Arc::clone(&runner),
            git.clone(),
            home.clone(),
            version,
        ));
        let repository = Arc::new(native::LocalRepositoryReader::new(
            Arc::clone(&runner),
            Arc::new(native::UnixRepositoryMarkerReader::new()),
            git.clone(),
            home.clone(),
            version,
        ));
        Self {
            coordinator: native::GitCoordinator::new(
                Arc::clone(&worktrees),
                repository,
                Arc::new(NativeWorktreeFilesystem),
            ),
            worktrees,
            directories,
            head_reader: Arc::new(HeadReader { runner, git, home }),
            inventory_identities: Arc::default(),
        }
    }
    fn checked_probe(
        &self,
        directory: &ServerDirectory,
        cancellation: &GitReadCancellation,
    ) -> Result<native::ProbeOutcome, GitFailure> {
        let source = self
            .directories
            .retain(directory)
            .map_err(directory_failure)?;
        let probe = self
            .coordinator
            .probe(&path(directory)?, cancellation)
            .map_err(read_failure)?;
        source
            .revalidate()
            .map_err(|_| GitFailure::IdentityReplaced)?;
        if matches!(probe, native::ProbeOutcome::NotRepository) {
            // Empty discovery output also accompanies inaccessible or invalid Git metadata.
            // Only a checked, available source with no enclosing Git marker is outside Git.
            let selected = path(directory)?
                .canonicalize()
                .map_err(|_| GitFailure::InvalidDirectory)?;
            for ancestor in selected.ancestors() {
                match std::fs::symlink_metadata(ancestor.join(".git")) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    _ => return Err(GitFailure::Unchecked),
                }
            }
            source
                .revalidate()
                .map_err(|_| GitFailure::IdentityReplaced)?;
        }
        Ok(probe)
    }
    fn inventory_lease(&self) -> HostInventoryLease {
        HostInventoryLease {
            registry: self.inventory_identities.clone(),
            identities: Vec::new(),
        }
    }
    fn retain_inventory_identity(&self, directory: &ServerDirectory) -> Result<(), GitFailure> {
        self.inventory_lease()
            .retain(&self.directories, directory, true)
    }
    /// Read Git's exact head and removal risks before the pure all-Workspace assessment.
    pub fn removal_facts(
        &self,
        repository: &RepositoryLocator,
        target: &spaceterm_core::organization::WorktreeRecord,
        cancellation: &GitReadCancellation,
    ) -> Result<spaceterm_core::worktrees::assessment::WorktreeRemovalFacts, GitFailure> {
        if target.repository != repository.scope().repository {
            return Err(GitFailure::WrongServer);
        }
        let snapshot = native::server::server_inventory(
            &self.worktrees,
            &path(&repository.parts().main_root)?,
            cancellation,
        )
        .map_err(read_failure)?;
        let location = path(&target.directory)?;
        let (index, record) = snapshot
            .worktrees
            .iter()
            .enumerate()
            .find(|(_, record)| native::server::same_location(&record.root, &location))
            .ok_or(GitFailure::UnlistedWorktree)?;
        if target.main != (index == 0) || target.missing != record.missing {
            return Err(GitFailure::GitStateChanged);
        }
        let (head, changes, submodules) = if record.missing {
            (None, false, false)
        } else {
            let check = self
                .coordinator
                .check_removal(&location, cancellation)
                .map_err(remove_failure)?;
            (
                self.head_reader.read(&location, cancellation)?,
                check.changes,
                check.submodules,
            )
        };
        Ok(
            spaceterm_core::worktrees::assessment::WorktreeRemovalFacts {
                target: target.clone(),
                listed: true,
                locked: record.locked,
                head,
                changes,
                submodules,
            },
        )
    }
    /// Expand only the owning server's preference and home; validate every literal branch operand.
    pub fn preview_creation(
        &self,
        draft: WorktreeCreateDraft,
        repository: &RepositoryLocator,
        settings: &ServerSettingsRecord,
        cancellation: &GitReadCancellation,
    ) -> Result<WorktreeCreationPreview, GitFailure> {
        if draft.parts().repository != repository.scope() {
            return Err(GitFailure::WrongServer);
        }
        let branch = branch(&draft.parts().branch)?;
        let root = path(&repository.parts().main_root)?;
        let branches = self
            .coordinator
            .branches(&root, cancellation)
            .map_err(read_failure)?;
        validate_available_branch(&branch, &branches)?;
        let snapshot = native::server::server_inventory(&self.worktrees, &root, cancellation)
            .map_err(read_failure)?;
        if snapshot.worktrees.iter().any(|record| matches!(&record.head, native::listing::WorktreeHead::Branch(name) if name == branch_name(&branch))) { return Err(GitFailure::BranchCheckedOut); }
        let (directory, version) = match &draft.parts().location {
            WorktreeLocationChoice::Explicit(directory) => (directory.clone(), None),
            WorktreeLocationChoice::ServerTemplate { expected_version } => {
                if *expected_version != settings.worktree_location.version {
                    return Err(GitFailure::StaleAssessment);
                }
                let repository_name = root
                    .file_name()
                    .and_then(|s| s.to_str())
                    .ok_or(GitFailure::InvalidDirectory)?;
                let expanded = native::path_template::expand(
                    settings.worktree_location.value.as_str(),
                    &self.head_reader.home,
                    repository_name,
                    branch_name(&branch),
                )
                .map_err(|_| GitFailure::InvalidTemplate)?;
                let directory = ServerDirectory::new(
                    repository.scope().server,
                    expanded.as_os_str().as_encoded_bytes().to_vec(),
                )
                .map_err(|_| GitFailure::InvalidDirectory)?;
                (directory, Some(*expected_version))
            }
        };
        self.directories
            .retain_create_target(&directory)
            .map_err(directory_failure)?;
        WorktreeCreationPreview::new(draft, directory, version)
    }
    /// Retain native repository and target identity before issuing a service assessment token.
    pub fn assess(
        &self,
        plan: ConfirmedGitPlan,
        cancellation: &GitReadCancellation,
    ) -> Result<Arc<dyn GitHostAssessment>, GitFailure> {
        self.assess_shared(Arc::new(plan), cancellation)
    }
    fn assess_shared(
        &self,
        plan: Arc<ConfirmedGitPlan>,
        cancellation: &GitReadCancellation,
    ) -> Result<Arc<dyn GitHostAssessment>, GitFailure> {
        plan.validate_scope()?;
        let roots = vec![
            self.directories
                .retain(&plan.repository.parts().main_root)
                .map_err(directory_failure)?,
            self.directories
                .retain(&plan.repository.parts().common_directory)
                .map_err(directory_failure)?,
        ];
        let inventory = native::server::server_inventory(
            &self.worktrees,
            &path(&plan.repository.parts().main_root)?,
            cancellation,
        )
        .map_err(read_failure)?;
        let source = inventory
            .worktrees
            .iter()
            .find(|record| {
                !record.missing && !matches!(record.head, native::listing::WorktreeHead::Bare)
            })
            // The existing hardened coordinator obtains authority from a live
            // working tree. Do not invent bare-root authority for this assessment.
            .ok_or_else(|| {
                if inventory.worktrees.first().is_some_and(|record| {
                    matches!(record.head, native::listing::WorktreeHead::Bare)
                }) {
                    GitFailure::BareRepositoryWithoutWorktree
                } else {
                    GitFailure::Unchecked
                }
            })?;
        let repository = self
            .coordinator
            .repository(&source.root, cancellation)
            .map_err(read_failure)?;
        let action = match &plan.action {
            ConfirmedGitAction::Create { preview, target } => {
                let target_lease = self
                    .directories
                    .retain_create_target(&preview.parts().directory)
                    .map_err(directory_failure)?;
                let actual = match target_lease.class() {
                    spaceterm_core::server_directories::DirectoryValidationClass::AbsentCreateTarget => CreationTargetObservation::Absent,
                    _ => CreationTargetObservation::EmptyDirectory,
                };
                if &actual != target {
                    return Err(GitFailure::StaleAssessment);
                }
                NativeAction::Create {
                    path: path(&preview.parts().directory)?,
                    branch: branch(&preview.parts().draft.parts().branch)?,
                    lease: Arc::new(target_lease),
                }
            }
            ConfirmedGitAction::Remove(scope) => {
                let location = path(&scope.facts().target.directory)?;
                let absence = if scope.facts().target.missing {
                    Some(Arc::new(
                        self.directories
                            .retain_absent_removal_target(&scope.facts().target.directory)
                            .map_err(directory_failure)?,
                    ))
                } else {
                    None
                };
                let expected = if scope.facts().target.missing {
                    let filesystem: Arc<dyn native::filesystem::WorktreeFilesystem> =
                        Arc::new(NativeWorktreeFilesystem);
                    if !filesystem
                        .is_absent(&location)
                        .map_err(|_| GitFailure::Unchecked)?
                    {
                        return Err(GitFailure::IdentityReplaced);
                    }
                    native::RemovalExpectation::Absent(filesystem)
                } else {
                    let check = self
                        .coordinator
                        .check_removal(&location, cancellation)
                        .map_err(remove_failure)?;
                    if check.changes != scope.facts().changes
                        || check.submodules != scope.facts().submodules
                    {
                        return Err(GitFailure::GitStateChanged);
                    }
                    if self.head_reader.read(&location, cancellation)? != scope.facts().head {
                        return Err(GitFailure::GitStateChanged);
                    }
                    native::RemovalExpectation::Worktree {
                        identity: check.identity,
                        require_clean: scope.approval().require_clean(),
                    }
                };
                NativeAction::Remove {
                    path: location,
                    force: scope.approval().force(),
                    expected,
                    absence,
                }
            }
        };
        Ok(Arc::new(NativeAssessment {
            plan,
            repository,
            action,
            coordinator: self.coordinator.clone(),
            reader: Arc::clone(&self.head_reader),
            roots,
            worktrees: Arc::clone(&self.worktrees),
        }))
    }
}
enum NativeAction {
    Create {
        path: PathBuf,
        branch: native::WorktreeBranch,
        lease: Arc<CreateTargetLease>,
    },
    Remove {
        path: PathBuf,
        force: bool,
        expected: native::RemovalExpectation,
        absence: Option<Arc<CreateTargetLease>>,
    },
}
struct NativeAssessment {
    plan: Arc<ConfirmedGitPlan>,
    repository: native::GitRepository,
    action: NativeAction,
    coordinator: native::GitCoordinator,
    reader: Arc<HeadReader>,
    roots: Vec<Arc<dyn native::filesystem::RetainedDirectory>>,
    worktrees: Arc<native::LocalWorktreeGit>,
}
impl GitHostAssessment for NativeAssessment {
    fn prepare(
        &self,
        intent: &GitWriteIntent,
        cancellation: &GitReadCancellation,
    ) -> Result<Box<dyn PreparedServerGitWrite>, GitFailure> {
        self.plan.validate_intent(intent)?;
        let (write, validation): (_, Arc<dyn FinalWriteValidation>) = match &self.action {
            NativeAction::Create {
                path,
                branch,
                lease,
            } => (
                native::GitWrite::Create {
                    path: path.clone(),
                    branch: branch.clone(),
                },
                Arc::new(CreateValidation(Arc::clone(lease))),
            ),
            NativeAction::Remove {
                path,
                force,
                expected,
                absence,
            } => (
                native::GitWrite::Remove {
                    path: path.clone(),
                    force: *force,
                    expected: expected.clone(),
                },
                Arc::new(RemoveValidation {
                    plan: self.plan.clone(),
                    reader: Arc::clone(&self.reader),
                    worktrees: Arc::clone(&self.worktrees),
                    expected: expected.clone(),
                    absence: absence.clone(),
                }),
            ),
        };
        let (observed, prepared) = self.coordinator.prepare_server_write(
            &self.repository,
            write,
            intent.binding(),
            cancellation,
            Arc::new(RepositoryValidation {
                roots: self.roots.clone(),
                validation,
            }),
        )?;
        if !same_directory(
            &observed.main_root,
            &path(&intent.parts().repository.parts().main_root)?,
        ) || !same_directory(
            &observed.common_directory,
            &path(&intent.parts().repository.parts().common_directory)?,
        ) {
            return Err(GitFailure::IdentityReplaced);
        }
        Ok(Box::new(NativePrepared {
            prepared,
            pending: observed,
            coordinator: self.coordinator.clone(),
            observation: HostWriteObservation {
                binding: intent.binding(),
                assessment_digest: intent.parts().assessment_digest,
            },
        }))
    }
}
struct RepositoryValidation {
    roots: Vec<Arc<dyn native::filesystem::RetainedDirectory>>,
    validation: Arc<dyn FinalWriteValidation>,
}
impl FinalWriteValidation for RepositoryValidation {
    fn validate(
        &self,
        cancellation: &GitReadCancellation,
        deadline: Instant,
    ) -> Result<(), GitFailure> {
        for root in &self.roots {
            root.revalidate()
                .map_err(|_| GitFailure::IdentityReplaced)?;
        }
        self.validation.validate(cancellation, deadline)?;
        for root in &self.roots {
            root.revalidate()
                .map_err(|_| GitFailure::IdentityReplaced)?;
        }
        Ok(())
    }
}
struct CreateValidation(Arc<CreateTargetLease>);
impl FinalWriteValidation for CreateValidation {
    fn validate(&self, _: &GitReadCancellation, _: Instant) -> Result<(), GitFailure> {
        self.0.revalidate().map_err(directory_failure)
    }
}
struct RemoveValidation {
    plan: Arc<ConfirmedGitPlan>,
    reader: Arc<HeadReader>,
    worktrees: Arc<native::LocalWorktreeGit>,
    expected: native::RemovalExpectation,
    absence: Option<Arc<CreateTargetLease>>,
}
impl FinalWriteValidation for RemoveValidation {
    fn validate(
        &self,
        cancellation: &GitReadCancellation,
        deadline: Instant,
    ) -> Result<(), GitFailure> {
        let ConfirmedGitAction::Remove(scope) = &self.plan.action else {
            return Err(GitFailure::InvalidResponse);
        };
        if let Some(absence) = &self.absence {
            absence.revalidate().map_err(directory_failure)?;
        }
        let worktrees = self.worktrees.with_read_deadline(deadline);
        let snapshot = native::server::server_inventory(
            &worktrees,
            &path(&self.plan.repository.parts().main_root)?,
            cancellation,
        )
        .map_err(read_failure)?;
        let location = path(&scope.facts().target.directory)?;
        worktrees
            .confirm_location(&location, &self.expected, cancellation)
            .map_err(remove_failure)?;
        let record = snapshot
            .worktrees
            .iter()
            .find(|w| native::server::same_location(&w.root, &location))
            .ok_or(GitFailure::UnlistedWorktree)?;
        if record.locked || record.missing != scope.facts().target.missing {
            return Err(GitFailure::GitStateChanged);
        }
        if !record.missing {
            let filesystem: Arc<dyn native::filesystem::WorktreeFilesystem> =
                Arc::new(NativeWorktreeFilesystem);
            let check = worktrees
                .check_removal(&location, &filesystem, cancellation)
                .map_err(remove_failure)?;
            if (check.changes && scope.approval().require_clean())
                || (check.submodules && !scope.facts().submodules)
                || self.reader.read_before(&location, cancellation, deadline)? != scope.facts().head
            {
                return Err(GitFailure::GitStateChanged);
            }
        }
        if let Some(absence) = &self.absence {
            absence.revalidate().map_err(directory_failure)?;
        }
        Ok(())
    }
}
struct NativePrepared {
    prepared: PreparedRepositoryWrite,
    observation: HostWriteObservation,
    pending: native::PendingWriteIntent,
    coordinator: native::GitCoordinator,
}
impl PreparedServerGitWrite for NativePrepared {
    fn observation(&self) -> &HostWriteObservation {
        &self.observation
    }
    fn execute(self: Box<Self>, permit: DurableGitStartPermit) -> HostGitCompletion {
        match self.prepared.execute(permit.binding()) {
            Ok(executed) => {
                let mut result = external(executed.outcome());
                match self
                    .coordinator
                    .reconcile(&self.pending, &GitReadCancellation::default())
                {
                    Ok(native::Reconciliation::ObservedApplied) => {
                        result.disposition = ExternalDisposition::DesiredStateObserved;
                    }
                    Ok(native::Reconciliation::ObservedNotApplied) => {
                        result.disposition = ExternalDisposition::NoEffectEstablished;
                        result
                            .failure
                            .get_or_insert(GitFailure::ExternalStateUncertain);
                    }
                    Ok(native::Reconciliation::NeedsReview) => {
                        result.disposition = ExternalDisposition::PartialOrAmbiguous;
                        result
                            .failure
                            .get_or_insert(GitFailure::ExternalStateUncertain);
                    }
                    Err(_) => {
                        result.disposition = ExternalDisposition::Unavailable;
                        result
                            .failure
                            .get_or_insert(GitFailure::ExternalStateUncertain);
                    }
                }
                HostGitCompletion {
                    result,
                    settlement: Some(Box::new(NativeSettlement(executed))),
                }
            }
            Err(failure) => HostGitCompletion {
                result: RetainedExternalResult {
                    disposition: if failure == GitFailure::ExternalStateUncertain {
                        ExternalDisposition::PartialOrAmbiguous
                    } else {
                        ExternalDisposition::NoEffectEstablished
                    },
                    failure: Some(failure),
                    ended_sessions: 0,
                },
                settlement: None,
            },
        }
    }
}
struct NativeSettlement(ExecutedRepositoryWrite);
impl GitHostSettlement for NativeSettlement {
    fn finish(self: Box<Self>, recorded: bool) -> Result<(), GitFailure> {
        self.0
            .finish(if recorded {
                Ok(())
            } else {
                Err(native::IntentError)
            })
            .map_err(native::server::write_failure)
            .map(|_| ())
    }
}
impl ServerGitHost for NativeServerGitHost {
    fn discover_source(
        &self,
        directory: &ServerDirectory,
        basis: &GitAuthoritySnapshot,
        cancellation: &GitReadCancellation,
    ) -> Result<Option<HostGitSource>, GitFailure> {
        let probe = self.checked_probe(directory, cancellation)?;
        let repository = match probe {
            native::ProbeOutcome::Repository(repository) => repository,
            native::ProbeOutcome::NotRepository | native::ProbeOutcome::Hidden => return Ok(None),
        };
        let (native::RepositoryRoot::Local(root), native::RepositoryRoot::Local(common)) =
            (&repository.root, &repository.common_directory)
        else {
            return Err(GitFailure::Unchecked);
        };
        let value = |path: &Path| {
            ServerDirectory::new(
                directory.server(),
                path.as_os_str().as_encoded_bytes().to_vec(),
            )
            .map_err(|_| GitFailure::InvalidDirectory)
        };
        let common_directory = basis
            .repositories
            .iter()
            .find(|locator| {
                native::server::same_location(
                    common,
                    &path(&locator.parts().common_directory).unwrap_or_default(),
                )
            })
            .map(|locator| locator.parts().common_directory.clone())
            .map_or_else(|| value(common), Ok)?;
        let worktree = basis
            .organization
            .state()
            .worktrees
            .iter()
            .find(|record| {
                native::server::same_location(root, &path(&record.directory).unwrap_or_default())
            })
            .map(|record| record.directory.clone())
            .map_or_else(|| value(root), Ok)?;
        Ok(Some(HostGitSource {
            common_directory,
            worktree,
        }))
    }
    fn status(
        &self,
        directory: &ServerDirectory,
        basis: &GitAuthoritySnapshot,
        cancellation: &GitReadCancellation,
    ) -> Result<HostGitStatus, GitFailure> {
        let repository = match self.checked_probe(directory, cancellation)? {
            native::ProbeOutcome::Repository(repository) => repository,
            native::ProbeOutcome::NotRepository | native::ProbeOutcome::Hidden => {
                return Ok(HostGitStatus {
                    inventory: None,
                    facts: empty_status(),
                });
            }
        };
        let native::RepositoryRoot::Local(root) = &repository.root else {
            return Err(GitFailure::Unchecked);
        };
        let summary = self
            .coordinator
            .count(root, repository.config.fsmonitor, cancellation)
            .map_err(read_failure)?;
        let facts = native::server::status_facts(std::num::NonZeroU64::MIN, &repository, &summary)?;
        let inventory = self.inventory(directory, basis, cancellation)?;
        Ok(HostGitStatus { inventory, facts })
    }
    fn inventory(
        &self,
        directory: &ServerDirectory,
        basis: &GitAuthoritySnapshot,
        cancellation: &GitReadCancellation,
    ) -> Result<Option<HostGitInventory>, GitFailure> {
        let source = self
            .directories
            .retain(directory)
            .map_err(directory_failure)?;
        let selected = path(directory)?;
        let snapshot = match self
            .coordinator
            .list(&selected, cancellation)
            .map_err(read_failure)?
        {
            Some(snapshot) => snapshot,
            None if basis.repositories.iter().any(|locator| {
                native::server::same_location(
                    &selected,
                    &path(&locator.parts().main_root).unwrap_or_default(),
                )
            }) =>
            {
                self.coordinator
                    .administrative_inventory(&selected, cancellation)
                    .map_err(read_failure)?
            }
            None => match self.checked_probe(directory, cancellation)? {
                native::ProbeOutcome::NotRepository | native::ProbeOutcome::Hidden => {
                    return Ok(None);
                }
                _ => return Err(GitFailure::Unchecked),
            },
        };
        source
            .revalidate()
            .map_err(|_| GitFailure::IdentityReplaced)?;
        if snapshot.worktrees.len() > MAX_GIT_REPOSITORIES {
            return Err(GitFailure::Capacity);
        }
        let value = |path: &Path| {
            ServerDirectory::new(
                directory.server(),
                path.as_os_str().as_encoded_bytes().to_vec(),
            )
            .map_err(|_| GitFailure::InvalidDirectory)
        };
        let existing = basis.repositories.iter().find(|locator| {
            native::server::same_location(
                &snapshot.common_directory,
                &path(&locator.parts().common_directory).unwrap_or_default(),
            )
        });
        let (main_root, common_directory) = match existing {
            Some(locator) => (
                locator.parts().main_root.clone(),
                locator.parts().common_directory.clone(),
            ),
            None => (
                value(&snapshot.main_root)?,
                value(&snapshot.common_directory)?,
            ),
        };
        let mut lease = self.inventory_lease();
        lease.retain(&self.directories, &main_root, existing.is_some())?;
        lease.retain(&self.directories, &common_directory, existing.is_some())?;
        let repository = existing.map(|r| r.scope().repository);
        let mut records = Vec::with_capacity(snapshot.worktrees.len());
        for (index, record) in snapshot.worktrees.iter().enumerate() {
            if cancellation.is_cancelled() {
                return Err(GitFailure::CancelledRead);
            }
            let retained = basis.organization.state().worktrees.iter().find(|old| {
                Some(old.repository) == repository
                    && native::server::same_location(
                        &record.root,
                        &path(&old.directory).unwrap_or_default(),
                    )
            });
            if retained
                .is_some_and(|old| old.main != (index == 0) || (old.missing && !record.missing))
            {
                return Err(GitFailure::IdentityReplaced);
            }
            let directory = retained
                .map(|old| old.directory.clone())
                .map_or_else(|| value(&record.root), Ok)?;
            if !record.missing {
                lease.retain(&self.directories, &directory, retained.is_some())?;
            } else {
                self.directories
                    .retain_absent_removal_target(&directory)
                    .map_err(directory_failure)?
                    .revalidate()
                    .map_err(directory_failure)?;
            }
            records.push(HostGitWorktree {
                retained: retained.map(|w| w.id),
                directory,
                main: index == 0,
                missing: record.missing,
                bare: matches!(record.head, native::listing::WorktreeHead::Bare),
                locked: record.locked,
                branch: match &record.head {
                    native::listing::WorktreeHead::Branch(name) => Some(branch_text(name)?),
                    _ => None,
                },
            });
        }
        source
            .revalidate()
            .map_err(|_| GitFailure::IdentityReplaced)?;
        Ok(Some(HostGitInventory {
            source: directory.clone(),
            current: snapshot.current,
            lease: Some(Arc::new(lease)),
            main_root,
            common_directory,
            repository,
            records,
        }))
    }
    fn branches(
        &self,
        repository: &RepositoryLocator,
        cancellation: &GitReadCancellation,
    ) -> Result<Vec<GitBranchRecord>, GitFailure> {
        let root = path(&repository.parts().main_root)?;
        self.retain_inventory_identity(&repository.parts().main_root)?;
        self.retain_inventory_identity(&repository.parts().common_directory)?;
        let branches = self
            .coordinator
            .branches(&root, cancellation)
            .map_err(read_failure)?;
        if branches.local.len() + branches.remote.len() > 16_384
            || branches
                .local
                .iter()
                .chain(&branches.remote)
                .map(String::len)
                .sum::<usize>()
                > 1024 * 1024
        {
            return Err(GitFailure::Capacity);
        }
        let inventory = native::server::server_inventory(&self.worktrees, &root, cancellation)
            .map_err(read_failure)?;
        let checked_out = |name: &str| {
            inventory.worktrees.iter().any(|w| matches!(&w.head, native::listing::WorktreeHead::Branch(branch) if branch == name))
        };
        branches
            .local
            .into_iter()
            .map(|name| {
                Ok(GitBranchRecord {
                    checked_out: checked_out(&name),
                    name: branch_text(&name)?,
                    remote: false,
                })
            })
            .chain(branches.remote.into_iter().map(|name| {
                Ok(GitBranchRecord {
                    checked_out: false,
                    name: branch_text(&name)?,
                    remote: true,
                })
            }))
            .collect()
    }
    fn creation_assessment(
        &self,
        draft: WorktreeCreateDraft,
        basis: &GitAuthoritySnapshot,
        cancellation: &GitReadCancellation,
    ) -> Result<(Arc<ConfirmedGitPlan>, Arc<dyn GitHostAssessment>), GitFailure> {
        let repository = read_locator(basis, draft.parts().repository)?.clone();
        let preview = self.preview_creation(
            draft,
            &repository,
            &basis.organization.state().settings,
            cancellation,
        )?;
        let target = match self
            .directories
            .retain_create_target(&preview.parts().directory)
            .map_err(directory_failure)?
            .class()
        {
            spaceterm_core::server_directories::DirectoryValidationClass::AbsentCreateTarget => {
                CreationTargetObservation::Absent
            }
            _ => CreationTargetObservation::EmptyDirectory,
        };
        let mut plan = ConfirmedGitPlan {
            repository,
            action: ConfirmedGitAction::Create { preview, target },
            assessment_digest: GitAssessmentDigest::from_bytes([1; 32]),
        };
        plan.assessment_digest = read_assessment_digest(&plan, basis.organization.epoch());
        let plan = Arc::new(plan);
        let native = self.assess_shared(Arc::clone(&plan), cancellation)?;
        Ok((plan, native))
    }
    fn removal_assessment(
        &self,
        worktree: spaceterm_core::identity::WorktreeId,
        basis: &GitAuthoritySnapshot,
        cancellation: &GitReadCancellation,
    ) -> Result<(Arc<ConfirmedGitPlan>, Arc<dyn GitHostAssessment>), GitFailure> {
        use spaceterm_core::worktrees::assessment::*;
        let target = basis
            .organization
            .state()
            .worktrees
            .iter()
            .find(|w| w.id == worktree)
            .ok_or(GitFailure::TargetGone)?;
        let repository = read_locator(
            basis,
            spaceterm_core::operations::RepositoryScope {
                server: worktree.server(),
                repository: target.repository,
            },
        )?
        .clone();
        let facts = self.removal_facts(&repository, target, cancellation)?;
        let inventory = self
            .inventory(&repository.parts().main_root, basis, cancellation)?
            .ok_or(GitFailure::UnlistedWorktree)?;
        let pinned =
            basis
                .organization
                .state()
                .workspaces
                .iter()
                .filter_map(|workspace| {
                    workspace.pinned_directory.value.as_ref().map(|directory| {
                        PinDirectoryContainment {
                            workspace: workspace.id,
                            directory: directory.clone(),
                            version: workspace.pinned_directory.version,
                            containment: self.containment(directory, &target.directory),
                        }
                    })
                })
                .collect();
        let panes = basis
            .organization
            .state()
            .panes
            .iter()
            .map(|pane| {
                let directory = basis
                    .runtime
                    .panes
                    .iter()
                    .find(|stamp| stamp.pane == pane.id)
                    .and_then(|stamp| stamp.confirmed_directory.as_ref())
                    .map_or_else(
                        || pane.restoration_directory().clone(),
                        |observation| observation.directory().clone(),
                    );
                PaneDirectoryContainment {
                    pane: pane.id,
                    containment: self.containment(&directory, &target.directory),
                    directory,
                }
            })
            .collect();
        let fallbacks = inventory
            .records
            .iter()
            .filter_map(|entry| {
                entry
                    .retained
                    .and_then(|id| {
                        basis
                            .organization
                            .state()
                            .worktrees
                            .iter()
                            .find(|w| w.id == id)
                    })
                    .map(|record| FallbackWorktree {
                        record: record.clone(),
                        bare: entry.bare,
                        available: !entry.missing
                            && self.directories.retain(&entry.directory).is_ok(),
                        containment: self.containment(&entry.directory, &target.directory),
                    })
            })
            .collect();
        let affected = AffectedWorktreeWork {
            pinned,
            panes,
            fallbacks,
        };
        let scope = assess_removal(&basis.organization, &basis.runtime, &facts, &affected)?;
        let mut plan = ConfirmedGitPlan {
            repository,
            action: ConfirmedGitAction::Remove(Box::new(scope)),
            assessment_digest: GitAssessmentDigest::from_bytes([1; 32]),
        };
        plan.assessment_digest = read_assessment_digest(&plan, basis.organization.epoch());
        let plan = Arc::new(plan);
        let native = self.assess_shared(Arc::clone(&plan), cancellation)?;
        Ok((plan, native))
    }
    fn containment(
        &self,
        directory: &ServerDirectory,
        root: &ServerDirectory,
    ) -> spaceterm_core::server_directories::DirectoryContainment {
        self.directories
            .containment(root, directory)
            .unwrap_or(spaceterm_core::server_directories::DirectoryContainment::Unchecked)
    }
    fn prepare_write(
        &self,
        request: HostGitWriteRequest,
        cancellation: &GitReadCancellation,
    ) -> Result<Box<dyn PreparedServerGitWrite>, GitFailure> {
        request.native.prepare(&request.intent, cancellation)
    }
    fn reconcile(
        &self,
        intent: &GitWriteIntent,
        cancellation: &GitReadCancellation,
    ) -> Result<HostRecoveryObservation, GitFailure> {
        let write = match &intent.parts().action {
            RetainedGitAction::Create {
                directory,
                branch: choice,
                ..
            } => native::DurableGitWrite::Create {
                path: path(directory)?,
                branch: branch(choice)?,
            },
            RetainedGitAction::Remove {
                directory,
                approval,
                ..
            } => native::DurableGitWrite::Remove {
                path: path(directory)?,
                force: approval.force(),
            },
        };
        let pending = native::PendingWriteIntent {
            main_root: path(&intent.parts().repository.parts().main_root)?,
            common_directory: path(&intent.parts().repository.parts().common_directory)?,
            write,
        };
        let observation = match self
            .coordinator
            .reconcile(&pending, cancellation)
            .map_err(read_failure)?
        {
            native::Reconciliation::ObservedApplied => RecoveryObservation::DesiredStateObserved,
            native::Reconciliation::ObservedNotApplied => RecoveryObservation::PreStateObserved,
            native::Reconciliation::NeedsReview => RecoveryObservation::PartialOrAmbiguous,
        };
        let disposition = match observation {
            RecoveryObservation::DesiredStateObserved => ExternalDisposition::DesiredStateObserved,
            RecoveryObservation::PreStateObserved => ExternalDisposition::NoEffectEstablished,
            _ => ExternalDisposition::PartialOrAmbiguous,
        };
        Ok(HostRecoveryObservation {
            observation,
            external: RetainedExternalResult {
                disposition,
                failure: (observation == RecoveryObservation::PartialOrAmbiguous)
                    .then_some(GitFailure::RecoveryNeedsReview),
                ended_sessions: 0,
            },
        })
    }
    fn shutdown(&self) -> Result<(), GitFailure> {
        self.coordinator.shutdown();
        self.inventory_identities
            .lock()
            .map_err(|_| GitFailure::Unchecked)?
            .entries
            .clear();
        Ok(())
    }
}
struct HeadReader {
    runner: Arc<dyn native::RepositoryProgramRunner>,
    git: PathBuf,
    home: PathBuf,
}
impl HeadReader {
    fn run(
        &self,
        directory: &Path,
        args: &[&str],
        cancellation: &GitReadCancellation,
        deadline: Instant,
    ) -> Result<(native::ProgramExit, Vec<u8>), GitFailure> {
        let request = native::ProgramRequest {
            executable: self.git.clone(),
            arguments: native::local_read::git_arguments(
                directory,
                native::FsmonitorPolicy::Disabled,
                args,
            ),
            directory: self.home.clone(),
            environment: native::local_read::git_environment(&self.git, &self.home),
            stdout_limit: Some(2048),
            deadline: Some(deadline),
            keep_process_group_on_exit: false,
        };
        let mut output = Vec::new();
        let exit = self
            .runner
            .run(
                &request,
                &mut |bytes| output.extend_from_slice(bytes),
                cancellation,
            )
            .map_err(|e| read_failure(native::local_read::repository_program_error(e)))?;
        if output.len() > 2048 {
            return Err(GitFailure::OutputTooLarge);
        }
        Ok((exit, output))
    }
    fn read(
        &self,
        directory: &Path,
        cancellation: &GitReadCancellation,
    ) -> Result<Option<RetainedGitHead>, GitFailure> {
        self.read_before(
            directory,
            cancellation,
            Instant::now() + Duration::from_secs(30),
        )
    }
    fn read_before(
        &self,
        directory: &Path,
        cancellation: &GitReadCancellation,
        deadline: Instant,
    ) -> Result<Option<RetainedGitHead>, GitFailure> {
        let (symbolic, name) = self.run(
            directory,
            &["symbolic-ref", "--quiet", "HEAD"],
            cancellation,
            deadline,
        )?;
        let branch = if symbolic.success() {
            let text = std::str::from_utf8(&name)
                .map_err(|_| GitFailure::InvalidResponse)?
                .strip_suffix('\n')
                .ok_or(GitFailure::InvalidResponse)?;
            Some(
                spaceterm_core::organization::BranchName::new(
                    text.strip_prefix("refs/heads/")
                        .ok_or(GitFailure::InvalidResponse)?
                        .to_owned(),
                )
                .map_err(|_| GitFailure::InvalidResponse)?,
            )
        } else {
            None
        };
        let (exit, object) = self.run(
            directory,
            &["rev-parse", "--verify", "HEAD"],
            cancellation,
            deadline,
        )?;
        if !exit.success() {
            return branch
                .map(RetainedGitHead::Unborn)
                .map(Some)
                .ok_or(GitFailure::Unchecked);
        }
        let object = object
            .strip_suffix(b"\n")
            .ok_or(GitFailure::InvalidResponse)?;
        let mut decoded = Vec::new();
        for pair in object.as_chunks::<2>().0 {
            let digit = |b: u8| {
                (b as char)
                    .to_digit(16)
                    .map(|v| v as u8)
                    .ok_or(GitFailure::InvalidResponse)
            };
            decoded.push((digit(pair[0])? << 4) | digit(pair[1])?);
        }
        let commit = match object.len() {
            40 => GitObjectId::Sha1(
                decoded
                    .try_into()
                    .map_err(|_| GitFailure::InvalidResponse)?,
            ),
            64 => GitObjectId::Sha256(
                decoded
                    .try_into()
                    .map_err(|_| GitFailure::InvalidResponse)?,
            ),
            _ => return Err(GitFailure::InvalidResponse),
        };
        Ok(Some(match branch {
            Some(name) => RetainedGitHead::Branch { name, commit },
            None => RetainedGitHead::Detached(commit),
        }))
    }
}
fn path(directory: &ServerDirectory) -> Result<PathBuf, GitFailure> {
    use std::os::unix::ffi::OsStringExt;
    let path = PathBuf::from(std::ffi::OsString::from_vec(directory.bytes().to_vec()));
    if !path.is_absolute()
        || directory
            .bytes()
            .split(|b| *b == b'/')
            .any(|p| p == b"." || p == b"..")
    {
        return Err(GitFailure::InvalidDirectory);
    }
    Ok(path)
}
pub fn branch(choice: &WorktreeBranchChoice) -> Result<native::WorktreeBranch, GitFailure> {
    let validate = |name: &spaceterm_core::organization::BranchName| {
        native::ref_format::validate_branch_name(name.as_str())
            .map_err(|_| GitFailure::InvalidBranch)
    };
    Ok(match choice {
        WorktreeBranchChoice::New { name, base } => {
            validate(name)?;
            let base = match base {
                BranchReference::Local(base) => {
                    validate(base)?;
                    native::BranchName::Local(base.as_str().into())
                }
                BranchReference::Remote(base) => {
                    validate(base)?;
                    native::BranchName::Remote(base.as_str().into())
                }
            };
            native::WorktreeBranch::New {
                name: name.as_str().into(),
                base,
            }
        }
        WorktreeBranchChoice::Existing { name } => {
            validate(name)?;
            native::WorktreeBranch::Existing {
                name: name.as_str().into(),
            }
        }
        WorktreeBranchChoice::Remote { remote, name } => {
            validate(remote)?;
            validate(name)?;
            native::WorktreeBranch::Remote {
                remote: remote.as_str().into(),
                name: name.as_str().into(),
            }
        }
    })
}
fn branch_name(branch: &native::WorktreeBranch) -> &str {
    match branch {
        native::WorktreeBranch::New { name, .. }
        | native::WorktreeBranch::Existing { name }
        | native::WorktreeBranch::Remote { name, .. } => name,
    }
}
fn validate_available_branch(
    branch: &native::WorktreeBranch,
    list: &native::BranchList,
) -> Result<(), GitFailure> {
    let exists = |base: &native::BranchName| match base {
        native::BranchName::Local(name) => list.local.contains(name),
        native::BranchName::Remote(name) => list.remote.contains(name),
    };
    match branch {
        native::WorktreeBranch::Existing { name } if !list.local.contains(name) => {
            Err(GitFailure::BranchMissing)
        }
        native::WorktreeBranch::New { name, .. } | native::WorktreeBranch::Remote { name, .. }
            if list.local.contains(name) =>
        {
            Err(GitFailure::BranchExists)
        }
        native::WorktreeBranch::New { base, .. } if !exists(base) => Err(GitFailure::BranchMissing),
        native::WorktreeBranch::Remote { remote, .. } if !list.remote.contains(remote) => {
            Err(GitFailure::BranchMissing)
        }
        _ => Ok(()),
    }
}
pub fn read_failure(error: native::RepositoryReadError) -> GitFailure {
    match error {
        native::RepositoryReadError::ToolMissing => GitFailure::ToolMissing,
        native::RepositoryReadError::Cancelled => GitFailure::CancelledRead,
        native::RepositoryReadError::TimedOut => GitFailure::TimedOutRead,
        native::RepositoryReadError::OutputTooLarge => GitFailure::OutputTooLarge,
        native::RepositoryReadError::InvalidResponse => GitFailure::InvalidResponse,
        _ => GitFailure::Unchecked,
    }
}
fn remove_failure(error: native::WorktreeRemoveError) -> GitFailure {
    match error {
        native::WorktreeRemoveError::HoldsPinnedDirectory => GitFailure::HoldsPinnedDirectory,
        native::WorktreeRemoveError::Replaced => GitFailure::IdentityReplaced,
        native::WorktreeRemoveError::Changed => GitFailure::GitStateChanged,
        native::WorktreeRemoveError::Unchecked => GitFailure::Unchecked,
        native::WorktreeRemoveError::Failed => GitFailure::WriteFailed,
    }
}
fn directory_failure(error: spaceterm_core::server_directories::DirectoryFailure) -> GitFailure {
    use spaceterm_core::server_directories::DirectoryFailure as D;
    match error {
        D::WrongServer => GitFailure::WrongServer,
        D::Capacity => GitFailure::Capacity,
        D::IdentityChanged => GitFailure::IdentityReplaced,
        D::NotDirectory => GitFailure::OccupiedTarget,
        D::InvalidDirectory => GitFailure::InvalidDirectory,
        _ => GitFailure::Unchecked,
    }
}
fn external(outcome: native::GitWriteOutcome) -> RetainedExternalResult {
    let (disposition, failure) = match outcome {
        native::GitWriteOutcome::Created | native::GitWriteOutcome::Removed => {
            (ExternalDisposition::DesiredStateObserved, None)
        }
        native::GitWriteOutcome::RepositoryChanged => (
            ExternalDisposition::NoEffectEstablished,
            Some(GitFailure::IdentityReplaced),
        ),
        native::GitWriteOutcome::RepositoryUnavailable => (
            ExternalDisposition::NoEffectEstablished,
            Some(GitFailure::Unchecked),
        ),
        native::GitWriteOutcome::CreateRejected(_)
        | native::GitWriteOutcome::RemoveRejected(_)
        | native::GitWriteOutcome::Uncertain => (
            ExternalDisposition::PartialOrAmbiguous,
            Some(GitFailure::WriteFailed),
        ),
    };
    RetainedExternalResult {
        disposition,
        failure,
        ended_sessions: 0,
    }
}

fn same_directory(left: &Path, right: &Path) -> bool {
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn empty_status() -> GitStatusFacts {
    GitStatusFacts {
        revision: std::num::NonZeroU64::MIN,
        branch: None,
        changes: vec![],
        dirty: false,
        unchecked: false,
        pull_request: None,
        upstream: None,
        operation: None,
        counts: None,
    }
}
fn branch_text(name: &str) -> Result<spaceterm_core::organization::BranchName, GitFailure> {
    spaceterm_core::organization::BranchName::new(name.to_owned())
        .map_err(|_| GitFailure::InvalidResponse)
}
fn read_locator(
    basis: &GitAuthoritySnapshot,
    scope: spaceterm_core::operations::RepositoryScope,
) -> Result<&RepositoryLocator, GitFailure> {
    basis
        .repositories
        .iter()
        .find(|r| {
            r.scope() == scope
                && basis
                    .organization
                    .state()
                    .repositories
                    .iter()
                    .any(|r| r.id == scope.repository)
        })
        .ok_or(GitFailure::TargetGone)
}

fn read_assessment_digest(
    plan: &ConfirmedGitPlan,
    epoch: spaceterm_core::authority::ServerEpoch,
) -> GitAssessmentDigest {
    use sha2::{Digest, Sha256};
    fn text(hash: &mut Sha256, value: &[u8]) {
        hash.update((value.len() as u64).to_le_bytes());
        hash.update(value);
    }
    fn number(hash: &mut Sha256, value: u64) {
        hash.update(value.to_le_bytes());
    }
    let mut hash = Sha256::new();
    hash.update(b"spaceterm-git-read-assessment-v1\0");
    hash.update(plan.repository.scope().server.bytes());
    number(&mut hash, epoch.get());
    number(&mut hash, plan.repository.scope().repository.local());
    text(&mut hash, plan.repository.parts().main_root.bytes());
    text(&mut hash, plan.repository.parts().common_directory.bytes());
    match &plan.action {
        ConfirmedGitAction::Create { preview, target } => {
            number(&mut hash, 1);
            text(&mut hash, preview.parts().directory.bytes());
            number(
                &mut hash,
                preview.parts().template_version.map_or(0, |v| v.get()),
            );
            number(
                &mut hash,
                match target {
                    CreationTargetObservation::Absent => 1,
                    CreationTargetObservation::EmptyDirectory => 2,
                },
            );
            match &preview.parts().draft.parts().branch {
                WorktreeBranchChoice::New { name, base } => {
                    number(&mut hash, 1);
                    text(&mut hash, name.as_str().as_bytes());
                    match base {
                        BranchReference::Local(name) => {
                            number(&mut hash, 1);
                            text(&mut hash, name.as_str().as_bytes());
                        }
                        BranchReference::Remote(name) => {
                            number(&mut hash, 2);
                            text(&mut hash, name.as_str().as_bytes());
                        }
                    }
                }
                WorktreeBranchChoice::Existing { name } => {
                    number(&mut hash, 2);
                    text(&mut hash, name.as_str().as_bytes());
                }
                WorktreeBranchChoice::Remote { remote, name } => {
                    number(&mut hash, 3);
                    text(&mut hash, remote.as_str().as_bytes());
                    text(&mut hash, name.as_str().as_bytes());
                }
            }
        }
        ConfirmedGitAction::Remove(scope) => {
            number(&mut hash, 2);
            hash.update(scope.assessment_digest().bytes());
        }
    }
    GitAssessmentDigest::from_bytes(hash.finalize().into())
}
