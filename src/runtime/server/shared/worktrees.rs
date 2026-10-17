//! Worktree writes reuse the retained mutation journal. A started write has no read cancellation.
use super::*;
use crate::repository_status::{
    ProgramError, ProgramExit, ProgramRequest, RepositoryProgramRunner, RepositoryToolDiscovery,
};
use crate::ssh::cancellation::SshCancellationToken;
use crate::worktrees::git::{LocalWorktreeGit, RemovalExpectation, WorktreeRemoveError};
use spaceterm_server::{
    hierarchy::{Mutation, Outcome},
    worktree::*,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

type PreparedWrite = (
    LocalWorktreeGit,
    Box<dyn FnOnce(LocalWorktreeGit) -> Result<(), Error> + Send>,
);

struct WriteRunner(Option<std::fs::File>);
impl RepositoryProgramRunner for WriteRunner {
    fn run(
        &self,
        request: &ProgramRequest,
        stdout: &mut dyn FnMut(&[u8]),
        token: &SshCancellationToken,
    ) -> Result<ProgramExit, ProgramError> {
        let mut request = request.clone();
        let write = request
            .arguments
            .windows(2)
            .any(|args| args[0] == "worktree" && (args[1] == "add" || args[1] == "remove"));
        if write {
            request.deadline = None;
            // Drain all output without allowing a noisy filter to cancel a started write.
            request.stdout_limit = None;
            crate::platform::unix_repository_program::UnixRepositoryProgramRunner::new().run_locked(
                &request,
                &mut |_| {},
                &SshCancellationToken::default(),
                self.0.as_ref(),
            )
        } else {
            request.deadline = Some(Instant::now() + Duration::from_secs(5));
            crate::platform::unix_repository_program::UnixRepositoryProgramRunner::new().run_locked(
                &request,
                stdout,
                token,
                self.0.as_ref(),
            )
        }
    }
}
fn git(lock: Option<std::fs::File>) -> Result<(LocalWorktreeGit, PathBuf), Error> {
    #[cfg(target_os = "macos")]
    let discovery = crate::platform::macos_repository_tools::MacosRepositoryToolDiscovery::new();
    #[cfg(target_os = "linux")]
    let discovery = crate::platform::linux_repository_tools::LinuxRepositoryToolDiscovery::new(
        std::env::var_os("PATH").as_deref(),
    );
    let tools = discovery.discover();
    let home = crate::platform::app_directories::AppDirectoryEnvironment::capture()
        .home
        .map(PathBuf::from)
        .ok_or(Error::Unavailable)?;
    let home = std::fs::canonicalize(home).map_err(|_| Error::Unavailable)?;
    let runner: Arc<dyn RepositoryProgramRunner> = Arc::new(WriteRunner(lock));
    let (status, _) = crate::repository_status::tool_check::check_tools(
        runner.as_ref(),
        &crate::repository_status::ToolInventory {
            git: tools.git.clone(),
            github_cli: None,
        },
        &home,
        &[],
        Instant::now() + Duration::from_secs(5),
        &SshCancellationToken::default(),
    );
    let (Some(path), crate::repository_status::GitToolStatus::Ready(version)) = (tools.git, status)
    else {
        return Err(Error::Unavailable);
    };
    Ok((
        LocalWorktreeGit::new(runner, path, home.clone(), version),
        home,
    ))
}
struct Approval {
    device: Id,
    review: Removal,
    expected: RemovalExpectation,
    record: crate::worktrees::listing::WorktreeRecord,
    expires: Instant,
}
pub(super) struct Writes {
    approvals: Vec<Approval>,
    worker: Option<std::thread::JoinHandle<Result<(), Error>>>,
    operation: Option<(Id, u64)>,
}
impl Writes {
    pub(super) fn new() -> Self {
        Self {
            approvals: Vec::new(),
            worker: None,
            operation: None,
        }
    }
}
impl Drop for Writes {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn list(git: &LocalWorktreeGit, root: &Path) -> Result<crate::worktrees::WorktreeSnapshot, Error> {
    let directory = local_filesystem()
        .validate_directory(root)
        .map_err(|_| Error::Unavailable)?;
    let list = git
        .list(directory.path(), &SshCancellationToken::default())
        .map_err(|_| Error::Unavailable)?
        .ok_or(Error::Missing)?;
    if list.repository.main_root() != root {
        return Err(Error::Changed);
    }
    Ok(list)
}
impl SharedWorkspaceHost {
    fn worktree_lock(&self) -> Result<Option<std::fs::File>, Refusal> {
        self.store
            .as_ref()
            .map(OrganizationStore::worktree_lock)
            .transpose()
            .map_err(|_| Refusal::StorageUnavailable)
    }
    fn affected_worktree_tabs(&self, root: &Path, path: &Path) -> Vec<AffectedTab> {
        self.inventory().affected_worktree_tabs(root, path)
    }

    fn pinned_in_worktree(&self, path: &Path) -> Result<bool, Error> {
        for pin in self
            .state
            .inventory
            .workspaces
            .iter()
            .filter_map(|w| w.pinned_directory.as_deref())
        {
            if pin.starts_with(path) {
                return Ok(true);
            }
            // Resolve symlinks on the owning server. Unreadable pins cannot authorize deletion.
            let pin = local_filesystem()
                .validate_directory(pin)
                .map_err(|_| Error::Unavailable)?;
            if local_filesystem().physically_contains(path, pin.path()) {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub(super) fn worktree_read(&mut self, device: Id, read: Read) -> Result<ReadReply, Error> {
        self.writes.approvals.retain(|a| a.expires > Instant::now());
        match read {
            Read::Context { root } => {
                let (git, home) = git(None)?;
                list(&git, &root)?;
                let branches = git
                    .branches(&root, &SshCancellationToken::default())
                    .map_err(|_| Error::Unavailable)?;
                if branches.local.len() + branches.remote.len() > 4096 {
                    return Err(Error::Unavailable);
                }
                Ok(ReadReply::Context {
                    settings: self.state.worktree_settings.clone().unwrap_or_default(),
                    home,
                    branches,
                })
            }
            Read::Settings => Ok(ReadReply::Settings(
                self.state.worktree_settings.clone().unwrap_or_default(),
            )),
            Read::SeedLocation { template } => {
                if self.state.worktree_settings.is_none() {
                    path_template::validate(&template).map_err(|_| Error::Location)?;
                    let mut next = self.state.clone();
                    next.worktree_settings = Some(Settings {
                        template,
                        version: 0,
                    });
                    self.commit(next).map_err(|_| Error::Unavailable)?;
                }
                Ok(ReadReply::Settings(
                    self.state.worktree_settings.clone().unwrap_or_default(),
                ))
            }
            Read::Branches { root } => {
                let (git, _) = git(None)?;
                list(&git, &root)?;
                let branches = git
                    .branches(&root, &SshCancellationToken::default())
                    .map_err(|_| Error::Unavailable)?;
                if branches.local.len() + branches.remote.len() > 4096 {
                    return Err(Error::Unavailable);
                }
                Ok(ReadReply::Branches(branches))
            }
            Read::Location {
                root,
                branch,
                location,
            } => {
                let (git, home) = git(None)?;
                list(&git, &root)?;
                let template = self
                    .state
                    .worktree_settings
                    .clone()
                    .unwrap_or_default()
                    .template;
                let location = location.map_or_else(
                    || {
                        path_template::expand(
                            &template,
                            &home,
                            root.file_name()
                                .and_then(|s| s.to_str())
                                .unwrap_or("repository"),
                            &branch,
                        )
                        .map_err(|_| Error::Location)
                    },
                    |s| {
                        let path = s
                            .strip_prefix("~/")
                            .map_or_else(|| PathBuf::from(&s), |s| home.join(s));
                        if path.is_absolute() {
                            Ok(path)
                        } else {
                            Err(Error::Location)
                        }
                    },
                )?;
                match local_filesystem().probe_new_directory(&location) {
                    Ok(crate::platform::local_filesystem::NewDirectoryTarget::Free) => {
                        Ok(ReadReply::Location(location))
                    }
                    _ => Err(Error::Location),
                }
            }
            Read::CheckRemoval { root, path } => {
                if !self.state.pending_worktrees().is_empty() {
                    return Err(Error::Unavailable);
                }
                let (git, _) = git(None)?;
                let list = list(&git, &root)?;
                let record = list
                    .worktrees
                    .iter()
                    .find(|r| r.root == path)
                    .ok_or(Error::Missing)?
                    .clone();
                if path == root {
                    return Err(Error::Main);
                }
                if record.locked {
                    return Err(Error::Locked);
                }
                if self.pinned_in_worktree(&path)? {
                    return Err(Error::Pinned);
                }
                let (expected, changes, submodules) = if matches!(
                    local_filesystem().validate_directory(&path),
                    Err(crate::platform::local_filesystem::LocalFilesystemError::Missing)
                ) {
                    (RemovalExpectation::Absent(local_filesystem()), false, false)
                } else {
                    let check = git
                        .check_removal(&path, &local_filesystem(), &SshCancellationToken::default())
                        .map_err(|_| Error::Unavailable)?;
                    (
                        RemovalExpectation::Worktree {
                            identity: check.identity,
                            require_clean: !check.changes,
                        },
                        check.changes,
                        check.submodules,
                    )
                };
                let review = Removal {
                    boot: self.prototype.boot,
                    confirmation: nonce().map_err(|_| Error::Unavailable)?,
                    root,
                    path,
                    changes,
                    submodules,
                    tabs: self.affected_worktree_tabs(list.repository.main_root(), &record.root),
                };
                self.writes.approvals.retain(|a| a.device != device);
                if self.writes.approvals.len() >= 32 {
                    return Err(Error::Unavailable);
                }
                self.writes.approvals.push(Approval {
                    device,
                    review: review.clone(),
                    expected,
                    record,
                    expires: Instant::now() + Duration::from_secs(300),
                });
                Ok(ReadReply::Removal(review))
            }
        }
    }
    pub(super) fn begin_worktree(
        &mut self,
        device: Id,
        sequence: u64,
        operation: Operation,
    ) -> Result<Reply, Refusal> {
        if !self.state.pending_worktrees().is_empty() {
            return Err(Refusal::Capacity);
        }
        let lock = self.worktree_lock()?;
        let mut next = self.state.clone();
        next.apply(
            device,
            sequence,
            Mutation::Worktree(operation.clone()),
            &self.prototype.directory,
            |_| false,
        )?;
        let checked = (|| -> Result<Option<PreparedWrite>, Error> {
            match &operation {
                Operation::SetLocation { version, template } => {
                    path_template::validate(template).map_err(|_| Error::Location)?;
                    let current = next.worktree_settings.clone().unwrap_or_default();
                    if *version != current.version {
                        return Err(Error::Changed);
                    }
                    next.worktree_settings = Some(Settings {
                        template: template.clone(),
                        version: current.version.checked_add(1).ok_or(Error::Unavailable)?,
                    });
                    Ok(None)
                }
                Operation::OpenTab {
                    workspace,
                    root,
                    path,
                } => {
                    let (git, _) = git(lock)?;
                    let list = list(&git, root)?;
                    if !list.worktrees.iter().any(|r| {
                        r.root == *path
                            && !r.missing
                            && r.head != crate::worktrees::listing::WorktreeHead::Bare
                    }) {
                        return Err(Error::Missing);
                    }
                    local_filesystem()
                        .validate_directory(path)
                        .map_err(|_| Error::Location)?;
                    let tab = next
                        .add_worktree_tab(device, *workspace, root.clone(), path.clone())
                        .map_err(|_| Error::Unavailable)?;
                    next.finish_worktree(device, sequence, Outcome::Accepted { target: tab })
                        .map_err(|_| Error::Unavailable)?;
                    Ok(None)
                }
                Operation::Create {
                    workspace,
                    root,
                    path,
                    branch,
                } => {
                    if !next.inventory.workspaces.iter().any(|w| w.id == *workspace) {
                        return Err(Error::Missing);
                    }
                    let (git, _) = git(lock)?;
                    list(&git, root)?;
                    if local_filesystem()
                        .probe_new_directory(path)
                        .map_err(|_| Error::Location)?
                        != crate::platform::local_filesystem::NewDirectoryTarget::Free
                    {
                        return Err(Error::Location);
                    }
                    // Reserve the first Tab before writing, so a successful Git write cannot exceed hierarchy capacity.
                    next.clone()
                        .add_worktree_tab(device, *workspace, root.clone(), path.clone())
                        .map_err(|_| Error::Unavailable)?;
                    let (root, path, branch) = (root.clone(), path.clone(), branch.clone());
                    Ok(Some((
                        git,
                        Box::new(move |git| {
                            git.create(&root, &path, &branch, &SshCancellationToken::default())
                                .map_err(|_| Error::Branch)
                        }),
                    )))
                }
                Operation::Remove {
                    root,
                    path,
                    boot,
                    confirmation,
                } => {
                    if *boot != self.prototype.boot {
                        return Err(Error::Changed);
                    }
                    let approval = self
                        .writes
                        .approvals
                        .iter()
                        .find(|a| {
                            a.device == device
                                && a.review.confirmation == *confirmation
                                && a.expires > Instant::now()
                        })
                        .ok_or(Error::Changed)?;
                    if approval.review.root != *root
                        || approval.review.path != *path
                        || approval.review.tabs != self.affected_worktree_tabs(root, path)
                    {
                        return Err(Error::Changed);
                    }
                    let (git, _) = git(lock)?;
                    let list = list(&git, root)?;
                    if path == root {
                        return Err(Error::Main);
                    }
                    if !list.worktrees.iter().any(|r| r == &approval.record) {
                        return Err(Error::Changed);
                    }
                    if self.pinned_in_worktree(path)? {
                        return Err(Error::Pinned);
                    }
                    git.confirm_location(
                        path,
                        &approval.expected,
                        &SshCancellationToken::default(),
                    )
                    .map_err(removal_error)?;
                    for affected in &approval.review.tabs {
                        let w = next
                            .inventory
                            .workspaces
                            .iter()
                            .find(|w| w.id == affected.workspace)
                            .ok_or(Error::Changed)?;
                        if w.tabs
                            .iter()
                            .all(|t| approval.review.tabs.iter().any(|a| a.tab == t.id))
                        {
                            let fallback = list
                                .worktrees
                                .iter()
                                .find(|r| {
                                    r.root != *path
                                        && !r.missing
                                        && r.head != crate::worktrees::listing::WorktreeHead::Bare
                                })
                                .ok_or(Error::Unavailable)?;
                            next.add_worktree_tab(
                                device,
                                w.id,
                                root.clone(),
                                fallback.root.clone(),
                            )
                            .map_err(|_| Error::Unavailable)?;
                        }
                        for pane in &affected.panes {
                            next.remove_pane(pane.id);
                        }
                    }
                    let (root, path, expected, force) = (
                        root.clone(),
                        path.clone(),
                        approval.expected.clone(),
                        approval.review.changes || approval.review.submodules,
                    );
                    Ok(Some((
                        git,
                        Box::new(move |git| {
                            git.remove(
                                &root,
                                &path,
                                force,
                                &expected,
                                &SshCancellationToken::default(),
                            )
                            .map_err(removal_error)
                        }),
                    )))
                }
            }
        })();
        match checked {
            Err(error) => {
                // Failed preflight retains no speculative Tab or setting change.
                next = self.state.clone();
                next.apply(
                    device,
                    sequence,
                    Mutation::Worktree(operation),
                    &self.prototype.directory,
                    |_| false,
                )?;
                next.finish_worktree(device, sequence, Outcome::WorktreeFailed(error))?;
                self.commit(next)?;
            }
            Ok(None) => {
                if next
                    .outcome(device, sequence)
                    .is_some_and(|o| o.outcome == Outcome::Pending)
                {
                    next.finish_worktree(device, sequence, Outcome::Accepted { target: device })?;
                }
                self.commit(next)?;
            }
            Ok(Some((git, run))) => {
                self.commit(next)?;
                // Persist the exact closure before ending its Sessions. Failed cleanup starts no write.
                self.synchronize()?;
                self.writes.operation = Some((device, sequence));
                self.writes.worker = Some(
                    std::thread::Builder::new()
                        .name("spaceterm-worktree-write".into())
                        .spawn(move || run(git))
                        .map_err(|_| Refusal::Capacity)?,
                );
                return Ok(Reply::MutationPending);
            }
        }
        Ok(Reply::Outcome(
            self.state
                .outcome(device, sequence)
                .ok_or(Refusal::Missing)?
                .clone(),
        ))
    }
    pub(super) fn finish_worktrees(&mut self, wait: bool) -> Result<(), Refusal> {
        if self
            .writes
            .worker
            .as_ref()
            .is_some_and(|w| wait || w.is_finished())
        {
            let result = self
                .writes
                .worker
                .take()
                .ok_or(Refusal::Failed)?
                .join()
                .unwrap_or(Err(Error::Interrupted));
            let (device, sequence) = self.writes.operation.take().ok_or(Refusal::Failed)?;
            self.settle_worktree(device, sequence, result)?;
        } else if self.writes.worker.is_none() {
            for (device, record) in self.state.pending_worktrees() {
                // The inherited file lock also covers an orphan Git child from a forced crash.
                let lock = self.worktree_lock()?;
                let (git, _) = git(lock).map_err(|_| Refusal::StorageUnavailable)?;
                if let Mutation::Worktree(
                    Operation::Create { root, .. } | Operation::Remove { root, .. },
                ) = &record.mutation
                {
                    list(&git, root).map_err(|_| Refusal::StorageUnavailable)?;
                }
                self.settle_worktree(device, record.sequence, Err(Error::Interrupted))?;
            }
        }
        Ok(())
    }
    fn settle_worktree(
        &mut self,
        device: Id,
        sequence: u64,
        mut result: Result<(), Error>,
    ) -> Result<(), Refusal> {
        let operation = self
            .state
            .outcome(device, sequence)
            .ok_or(Refusal::Missing)?
            .mutation
            .clone();
        let mut next = self.state.clone();
        let mut created_path = None;
        if let Mutation::Worktree(Operation::Create { root, .. } | Operation::Remove { root, .. }) =
            &operation
        {
            let (git, _) = git(None).map_err(|_| Refusal::StorageUnavailable)?;
            let snapshot = list(&git, root).map_err(|_| Refusal::StorageUnavailable)?;
            if result.is_ok()
                && let Mutation::Worktree(Operation::Create { path, branch, .. }) = &operation
            {
                created_path = local_filesystem()
                    .validate_directory(path)
                    .ok()
                    .and_then(|d| std::fs::canonicalize(d.path()).ok())
                    .filter(|path| {
                        snapshot.worktrees.iter().any(|r| {
                            r.root == *path
                                && !r.missing
                                && r.head
                                    == crate::worktrees::listing::WorktreeHead::Branch(
                                        branch.name().into(),
                                    )
                        })
                    });
                if created_path.is_none() {
                    result = Err(Error::Changed);
                }
            }
            self.repositories.publish_worktrees(&snapshot);
        }
        let target = if let Mutation::Worktree(Operation::Create {
            workspace, root, ..
        }) = &operation
        {
            if result.is_ok() {
                next.add_worktree_tab(
                    device,
                    *workspace,
                    root.clone(),
                    created_path.ok_or(Refusal::DirectoryUnavailable)?,
                )?
            } else {
                device
            }
        } else {
            device
        };
        next.finish_worktree(
            device,
            sequence,
            match result {
                Ok(()) => Outcome::Accepted { target },
                Err(e) => Outcome::WorktreeFailed(e),
            },
        )?;

        self.commit(next)
    }
}
fn removal_error(error: WorktreeRemoveError) -> Error {
    match error {
        WorktreeRemoveError::HoldsPinnedDirectory => Error::Pinned,
        WorktreeRemoveError::Replaced | WorktreeRemoveError::Changed => Error::Changed,
        _ => Error::Unavailable,
    }
}
