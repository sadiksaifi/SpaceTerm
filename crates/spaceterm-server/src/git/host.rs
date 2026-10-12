//! Blocking native Git bridge. It reuses hardened host execution and the existing serializer.
use super::authority::{ConfirmedGitAction, ConfirmedGitPlan};
use super::writes::DurableGitStartPermit;
use spaceterm_core::operations::OperationBinding;
use spaceterm_core::organization::{ServerDirectory, ServerSettingsRecord};
use spaceterm_core::worktrees::*;
use spaceterm_host::git as native;
use spaceterm_host::git::filesystem::NativeWorktreeFilesystem;
use spaceterm_host::git::server::{
    ExecutedRepositoryWrite, FinalWriteValidation, PreparedRepositoryWrite,
};
use spaceterm_host::server_directories::native::NativeDirectoryProvider;
use spaceterm_host::server_directories::validation::CreateTargetLease;
use std::path::{Path, PathBuf};
use std::sync::Arc;
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
pub trait ServerGitHost: Send + Sync {
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

pub struct NativeServerGitHost {
    coordinator: native::GitCoordinator,
    worktrees: Arc<native::LocalWorktreeGit>,
    directories: Arc<NativeDirectoryProvider>,
    head_reader: Arc<HeadReader>,
}
impl NativeServerGitHost {
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
        }
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
            .find(|(_, record)| record.root == location)
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
            .ok_or(GitFailure::Unchecked)?;
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
    },
}
struct NativeAssessment {
    plan: ConfirmedGitPlan,
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
            } => (
                native::GitWrite::Remove {
                    path: path.clone(),
                    force: *force,
                    expected: expected.clone(),
                },
                Arc::new(RemoveValidation {
                    plan: self.plan.clone(),
                    coordinator: self.coordinator.clone(),
                    reader: Arc::clone(&self.reader),
                    worktrees: Arc::clone(&self.worktrees),
                    expected: expected.clone(),
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
    fn validate(&self) -> Result<(), GitFailure> {
        for root in &self.roots {
            root.revalidate()
                .map_err(|_| GitFailure::IdentityReplaced)?;
        }
        self.validation.validate()?;
        for root in &self.roots {
            root.revalidate()
                .map_err(|_| GitFailure::IdentityReplaced)?;
        }
        Ok(())
    }
}
struct CreateValidation(Arc<CreateTargetLease>);
impl FinalWriteValidation for CreateValidation {
    fn validate(&self) -> Result<(), GitFailure> {
        self.0.revalidate().map_err(directory_failure)
    }
}
struct RemoveValidation {
    plan: ConfirmedGitPlan,
    coordinator: native::GitCoordinator,
    reader: Arc<HeadReader>,
    worktrees: Arc<native::LocalWorktreeGit>,
    expected: native::RemovalExpectation,
}
impl FinalWriteValidation for RemoveValidation {
    fn validate(&self) -> Result<(), GitFailure> {
        let ConfirmedGitAction::Remove(scope) = &self.plan.action else {
            return Err(GitFailure::InvalidResponse);
        };
        let cancellation = GitReadCancellation::default();
        let snapshot = native::server::server_inventory(
            &self.worktrees,
            &path(&self.plan.repository.parts().main_root)?,
            &cancellation,
        )
        .map_err(read_failure)?;
        let location = path(&scope.facts().target.directory)?;
        self.worktrees
            .confirm_location(&location, &self.expected, &cancellation)
            .map_err(remove_failure)?;
        let record = snapshot
            .worktrees
            .iter()
            .find(|w| w.root == location)
            .ok_or(GitFailure::UnlistedWorktree)?;
        if record.locked || record.missing != scope.facts().target.missing {
            return Err(GitFailure::GitStateChanged);
        }
        if !record.missing {
            let check = self
                .coordinator
                .check_removal(&location, &cancellation)
                .map_err(remove_failure)?;
            if (check.changes && scope.approval().require_clean())
                || (check.submodules && !scope.facts().submodules)
                || self.reader.read(&location, &cancellation)? != scope.facts().head
            {
                return Err(GitFailure::GitStateChanged);
            }
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
            deadline: Some(Instant::now() + Duration::from_secs(30)),
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
        let (symbolic, name) = self.run(
            directory,
            &["symbolic-ref", "--quiet", "HEAD"],
            cancellation,
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
        let (exit, object) =
            self.run(directory, &["rev-parse", "--verify", "HEAD"], cancellation)?;
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
