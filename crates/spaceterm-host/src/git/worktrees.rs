//! Runs the discovered git for Worktrees, with the same hardening Repository Status reads use.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use thiserror::Error;

use super::CancellationToken;
use super::WorktreeSnapshot;
use super::filesystem::{DirectoryError, RetainedDirectory, WorktreeFilesystem};
use super::listing::{WorktreeHead, WorktreeRecord, parse_worktree_list};
use super::ref_format::validate_branch_name;
use crate::git::discovery::{Discovery, parse_discovery};
use crate::git::local_read::{
    DISCOVERY_ARGUMENTS, git_arguments, git_environment, repository_program_error,
};
use crate::git::{
    FsmonitorPolicy, ProgramExit, ProgramRequest, RepositoryProgramRunner, RepositoryReadError,
    ToolVersion,
};

/// The oldest git whose `worktree list --porcelain` accepts `-z`.
const NUL_LIST_VERSION: ToolVersion = ToolVersion {
    major: 2,
    minor: 36,
    patch: 0,
};
/// Discovery prints a few short paths; a listing holds a few hundred bytes per Worktree.
const OUTPUT_LIMIT: usize = 1024 * 1024;

/// The branches a new Worktree can check out or start from, most recently committed first.
#[derive(Clone, Default, Eq, PartialEq)]
pub struct BranchList {
    /// Local branch names, such as `feature/login`.
    pub local: Vec<String>,
    /// Remote-tracking branch names already fetched, such as `origin/main`.
    pub remote: Vec<String>,
}

impl std::fmt::Debug for BranchList {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BranchList")
            .field("local", &self.local.len())
            .field("remote", &self.remote.len())
            .finish()
    }
}

/// The branch a new Worktree checks out.
#[derive(Clone, Eq, PartialEq)]
pub enum WorktreeBranch {
    /// A new branch `name` starting at `base`, a local or remote-tracking branch name. The new
    /// branch tracks nothing.
    New {
        /// Name of the new local branch.
        name: String,
        /// Existing branch whose commit starts the new branch.
        base: BranchName,
    },
    /// A local branch no Worktree has checked out.
    Existing {
        /// Name of the existing local branch.
        name: String,
    },
    /// A new local branch `name` that tracks the remote-tracking branch `remote`.
    Remote {
        /// Remote-tracking branch already fetched on this host.
        remote: String,
        /// Name of the new local tracking branch.
        name: String,
    },
}

impl std::fmt::Debug for WorktreeBranch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::New { .. } => "New",
            Self::Existing { .. } => "Existing",
            Self::Remote { .. } => "Remote",
        })
    }
}

/// A local or remote-tracking branch, by its short name.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum BranchName {
    /// A repository or branch on the owning host.
    Local(String),
    /// An opaque remote machine, path, or fetched branch spelling.
    Remote(String),
}

impl BranchName {
    fn full_name(&self) -> String {
        match self {
            Self::Local(name) => format!("refs/heads/{name}"),
            Self::Remote(name) => format!("refs/remotes/{name}"),
        }
    }
}

/// Why git did not create a Worktree. Checks before the write classify it, because the runner
/// discards git's messages.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum WorktreeCreateError {
    #[error("git refuses the branch name")]
    /// Git refuses the branch name.
    InvalidName,
    #[error("a branch with the name already exists")]
    /// A branch with the name already exists.
    BranchExists,
    #[error("another Worktree has the branch checked out")]
    /// Another Worktree has the branch checked out.
    BranchCheckedOut,
    #[error("the branch no longer exists")]
    /// The branch no longer exists.
    BranchMissing,
    #[error("git could not create the Worktree")]
    /// Git could not create the Worktree.
    Failed,
}

/// Why git did not remove a Worktree.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum WorktreeRemoveError {
    #[error("the Worktree holds a Pinned Directory")]
    /// The Worktree holds a Pinned Directory.
    HoldsPinnedDirectory,
    #[error("a different Worktree now has the confirmed location")]
    /// A different Worktree now has the confirmed location.
    Replaced,
    #[error("the Worktree changed after removal was confirmed")]
    /// The Worktree changed after removal was confirmed.
    Changed,
    #[error("the Worktree could not be checked before removal")]
    /// The Worktree could not be checked before removal.
    Unchecked,
    #[error("git could not remove the Worktree")]
    /// Git could not remove the Worktree.
    Failed,
}

/// One Worktree as it was when the person was asked to remove it: git's administrative
/// directory for it and its retained directory. Git names the administrative directory after the
/// Worktree's folder, so a Worktree recreated at the same location can reuse it; the retained
/// directory tells that replacement apart.
#[derive(Clone)]
pub struct WorktreeIdentity {
    administrative: String,
    directory: Arc<dyn RetainedDirectory>,
}

impl std::fmt::Debug for WorktreeIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("WorktreeIdentity(..)")
    }
}

/// What the person saw at a Worktree's location when they confirmed its removal. A removal
/// acts only while the location still shows it.
#[derive(Clone)]
pub enum RemovalExpectation {
    /// The checked Worktree.
    Worktree {
        /// Retained physical and administrative identity of the confirmed Worktree.
        identity: WorktreeIdentity,
        /// Refuse removal if tracked or untracked changes appeared after confirmation.
        require_clean: bool,
    },
    /// No directory: git lists the Worktree as missing.
    Absent(Arc<dyn WorktreeFilesystem>),
}

impl std::fmt::Debug for RemovalExpectation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Worktree { .. } => "RemovalExpectation::Worktree(..)",
            Self::Absent(_) => "RemovalExpectation::Absent(..)",
        })
    }
}

/// What removing a Worktree would act on, read before the person confirms.
#[derive(Clone, Debug)]
pub struct RemovalCheck {
    /// Retained physical and administrative identity of the confirmed Worktree.
    pub identity: WorktreeIdentity,
    /// Removing it discards modified tracked files or untracked files.
    pub changes: bool,
    /// It holds a checked-out submodule, which git removes only with `--force`.
    pub submodules: bool,
}

/// Runs git for one machine's Worktrees. Blocking; callers use a dedicated thread.
pub struct LocalWorktreeGit {
    runner: Arc<dyn RepositoryProgramRunner>,
    git: PathBuf,
    physical_home: PathBuf,
    git_version: ToolVersion,
}

impl LocalWorktreeGit {
    /// Construct Worktree execution with discovered Git and an explicit physical home.
    pub fn new(
        runner: Arc<dyn RepositoryProgramRunner>,
        git: PathBuf,
        physical_home: PathBuf,
        git_version: ToolVersion,
    ) -> Self {
        Self {
            runner,
            git,
            physical_home,
            git_version,
        }
    }

    /// Lists the Worktrees of the repository containing `directory`, or `None` when no work tree
    /// repository shows for it.
    pub fn list(
        &self,
        directory: &Path,
        cancellation: &CancellationToken,
    ) -> Result<Option<WorktreeSnapshot>, RepositoryReadError> {
        let directory_text = directory.to_str().ok_or(RepositoryReadError::Unavailable)?;
        let mut output = Vec::new();
        let exit = self.run(
            directory,
            git_arguments(directory, FsmonitorPolicy::Disabled, &DISCOVERY_ARGUMENTS),
            &mut output,
            cancellation,
        )?;
        let repository = match parse_discovery(
            &output,
            directory_text,
            &self.physical_home.to_string_lossy(),
        )? {
            Discovery::NotRepository | Discovery::Hidden => return Ok(None),
            Discovery::Repository(repository) => repository,
        };
        if !exit.success() {
            return Err(RepositoryReadError::Unavailable);
        }
        let current = PathBuf::from(&*repository.toplevel);
        let worktrees = self.list_records(&current, cancellation)?;
        Ok(Some(snapshot(
            worktrees,
            current,
            PathBuf::from(&*repository.common_directory),
        )))
    }

    /// Inspect a known repository root, including a bare root retained for linked Worktrees.
    /// Presentation discovery deliberately hides bare repositories and is not suitable here.
    pub(super) fn administrative_inventory(
        &self,
        root: &Path,
        cancellation: &CancellationToken,
    ) -> Result<WorktreeSnapshot, RepositoryReadError> {
        let mut output = Vec::new();
        let exit = self.git(
            root,
            &["rev-parse", "--git-common-dir"],
            &mut output,
            cancellation,
        )?;
        if !exit.success() {
            return Err(RepositoryReadError::Unavailable);
        }
        let common = std::str::from_utf8(&output)
            .ok()
            .and_then(|text| text.strip_suffix('\n'))
            .filter(|path| !path.is_empty() && !path.contains(['\n', '\0']))
            .ok_or(RepositoryReadError::InvalidResponse)?;
        let worktrees = self.list_records(root, cancellation)?;
        Ok(snapshot(worktrees, root.to_owned(), root.join(common)))
    }

    /// Lists the local and fetched remote-tracking branches of the repository at `root`.
    pub fn branches(
        &self,
        root: &Path,
        cancellation: &CancellationToken,
    ) -> Result<BranchList, RepositoryReadError> {
        let mut output = Vec::new();
        let exit = self.git(root, &BRANCH_ARGUMENTS, &mut output, cancellation)?;
        if !exit.success() {
            return Err(RepositoryReadError::Unavailable);
        }
        parse_branches(&output)
    }

    /// Creates a Worktree at `path` from the repository at `root`. Git creates missing parent
    /// directories. Hooks do not run, so no `post-checkout` setup happens.
    pub fn create(
        &self,
        root: &Path,
        path: &Path,
        branch: &WorktreeBranch,
        cancellation: &CancellationToken,
    ) -> Result<(), WorktreeCreateError> {
        let path = path.to_str().ok_or(WorktreeCreateError::Failed)?;
        match branch {
            WorktreeBranch::New { name, base } => {
                self.require_new_branch(root, name, cancellation)?;
                let base = base.full_name();
                self.require_ref(root, &base, cancellation)?;
                self.add(
                    root,
                    &["--no-track", "-b", name, "--", path, &base],
                    cancellation,
                )
            }
            WorktreeBranch::Existing { name } => {
                self.require_ref(root, &format!("refs/heads/{name}"), cancellation)?;
                if self.checked_out(root, name, cancellation)? {
                    return Err(WorktreeCreateError::BranchCheckedOut);
                }
                self.add(root, &["--", path, name], cancellation)
            }
            WorktreeBranch::Remote { remote, name } => {
                self.require_new_branch(root, name, cancellation)?;
                let remote = format!("refs/remotes/{remote}");
                self.require_ref(root, &remote, cancellation)?;
                self.add(
                    root,
                    &["--track", "-b", name, "--", path, &remote],
                    cancellation,
                )
            }
        }
    }

    /// Reads which Worktree is at `path` and whether it has changes that removing it discards:
    /// modified tracked files or untracked files. Git removes ignored files without asking, so
    /// they don't count.
    pub fn check_removal(
        &self,
        path: &Path,
        filesystem: &Arc<dyn WorktreeFilesystem>,
        cancellation: &CancellationToken,
    ) -> Result<RemovalCheck, WorktreeRemoveError> {
        let directory = filesystem
            .validate_directory(path)
            .map_err(|_| WorktreeRemoveError::Failed)?;
        let identity = WorktreeIdentity {
            administrative: self.administrative_directory(path, cancellation)?,
            directory,
        };
        let changes = self.has_changes(path, cancellation)?;
        let submodules =
            self.has_submodules(path, &identity.administrative, filesystem, cancellation);
        Ok(RemovalCheck {
            identity,
            changes,
            submodules,
        })
    }

    fn has_changes(
        &self,
        path: &Path,
        cancellation: &CancellationToken,
    ) -> Result<bool, WorktreeRemoveError> {
        let mut output = Vec::new();
        match self.git(path, &CHANGES_ARGUMENTS, &mut output, cancellation) {
            Ok(exit) if exit.success() => Ok(!output.is_empty()),
            // More changes than the output limit holds are still changes.
            Err(RepositoryReadError::OutputTooLarge) => Ok(true),
            _ => Err(WorktreeRemoveError::Failed),
        }
    }

    /// Whether the Worktree at `path` holds a checked-out submodule. As git judges it, one does
    /// when the Worktree's administrative directory keeps submodule repositories, or when a path
    /// `.gitmodules` names has a `.git`.
    fn has_submodules(
        &self,
        path: &Path,
        administrative: &str,
        filesystem: &Arc<dyn WorktreeFilesystem>,
        cancellation: &CancellationToken,
    ) -> bool {
        if filesystem
            .validate_directory(&Path::new(administrative).join("modules"))
            .is_ok()
        {
            return true;
        }
        let mut output = Vec::new();
        match self.git(path, &SUBMODULE_PATH_ARGUMENTS, &mut output, cancellation) {
            Ok(exit) if exit.success() => {}
            // No `.gitmodules`, or none of its entries names a path.
            _ => return false,
        }
        output
            .split(|byte| *byte == 0)
            .filter_map(|entry| entry.splitn(2, |byte| *byte == b'\n').nth(1))
            .filter_map(|submodule| std::str::from_utf8(submodule).ok())
            .map(Path::new)
            .filter(|submodule| {
                submodule
                    .components()
                    .all(|component| matches!(component, std::path::Component::Normal(_)))
            })
            .any(|submodule| filesystem.has_entry(&path.join(submodule).join(".git")))
    }

    /// Whether the Worktree at `path` is still the one `expected` identifies: the same directory,
    /// which git still administers from the same place.
    pub fn is_worktree(
        &self,
        path: &Path,
        expected: &WorktreeIdentity,
        cancellation: &CancellationToken,
    ) -> Result<(), WorktreeRemoveError> {
        if expected.directory.path() != path {
            return Err(WorktreeRemoveError::Replaced);
        }
        match expected.directory.revalidate() {
            Ok(_) => {}
            Err(DirectoryError::IdentityChanged) => {
                return Err(WorktreeRemoveError::Replaced);
            }
            Err(_) => return Err(WorktreeRemoveError::Failed),
        }
        if self.administrative_directory(path, cancellation)? == expected.administrative {
            Ok(())
        } else {
            Err(WorktreeRemoveError::Replaced)
        }
    }

    /// Whether `path` still shows what the person confirmed: the same Worktree, or still no
    /// directory for a missing one.
    pub fn confirm_location(
        &self,
        path: &Path,
        expected: &RemovalExpectation,
        cancellation: &CancellationToken,
    ) -> Result<(), WorktreeRemoveError> {
        match expected {
            RemovalExpectation::Worktree {
                identity,
                require_clean,
            } => {
                self.is_worktree(path, identity, cancellation)?;
                if *require_clean
                    && self
                        .has_changes(path, cancellation)
                        .map_err(|_| WorktreeRemoveError::Unchecked)?
                {
                    return Err(WorktreeRemoveError::Changed);
                }
                Ok(())
            }
            RemovalExpectation::Absent(filesystem) => match filesystem.is_absent(path) {
                Ok(true) => Ok(()),
                Ok(false) => Err(WorktreeRemoveError::Replaced),
                Err(_) => Err(WorktreeRemoveError::Failed),
            },
        }
    }

    /// Removes the Worktree at `path` and keeps its branch. Without `force`, git refuses a
    /// Worktree with changes. It first checks that `path` still shows what the person confirmed,
    /// so a confirmation never removes a replacement.
    pub fn remove(
        &self,
        root: &Path,
        path: &Path,
        force: bool,
        expected: &RemovalExpectation,
        cancellation: &CancellationToken,
    ) -> Result<(), WorktreeRemoveError> {
        self.confirm_location(path, expected, cancellation)?;
        let path = path.to_str().ok_or(WorktreeRemoveError::Failed)?;
        let mut arguments = vec!["worktree", "remove"];
        if force {
            arguments.push("--force");
        }
        arguments.extend(["--", path]);
        match self.git(root, &arguments, &mut Vec::new(), cancellation) {
            Ok(exit) if exit.success() => Ok(()),
            _ => Err(WorktreeRemoveError::Failed),
        }
    }

    fn administrative_directory(
        &self,
        path: &Path,
        cancellation: &CancellationToken,
    ) -> Result<String, WorktreeRemoveError> {
        let mut output = Vec::new();
        match self.git(path, &IDENTITY_ARGUMENTS, &mut output, cancellation) {
            Ok(exit) if exit.success() => String::from_utf8(output)
                .ok()
                .map(|directory| directory.trim_end_matches('\n').to_owned())
                .filter(|directory| !directory.is_empty())
                .ok_or(WorktreeRemoveError::Failed),
            _ => Err(WorktreeRemoveError::Failed),
        }
    }

    fn add(
        &self,
        root: &Path,
        options: &[&str],
        cancellation: &CancellationToken,
    ) -> Result<(), WorktreeCreateError> {
        let mut arguments = vec!["worktree", "add"];
        arguments.extend_from_slice(options);
        match self.git(root, &arguments, &mut Vec::new(), cancellation) {
            Ok(exit) if exit.success() => Ok(()),
            _ => Err(WorktreeCreateError::Failed),
        }
    }

    /// Checks that git accepts `name` for a new branch and that no branch has it.
    fn require_new_branch(
        &self,
        root: &Path,
        name: &str,
        cancellation: &CancellationToken,
    ) -> Result<(), WorktreeCreateError> {
        validate_branch_name(name).map_err(|_| WorktreeCreateError::InvalidName)?;
        let format = self
            .git(
                root,
                &["check-ref-format", "--branch", name],
                &mut Vec::new(),
                cancellation,
            )
            .map_err(|_| WorktreeCreateError::Failed)?;
        if !format.success() {
            return Err(WorktreeCreateError::InvalidName);
        }
        match self.ref_exists(root, &format!("refs/heads/{name}"), cancellation)? {
            true => Err(WorktreeCreateError::BranchExists),
            false => Ok(()),
        }
    }

    fn require_ref(
        &self,
        root: &Path,
        full_name: &str,
        cancellation: &CancellationToken,
    ) -> Result<(), WorktreeCreateError> {
        match self.ref_exists(root, full_name, cancellation)? {
            true => Ok(()),
            false => Err(WorktreeCreateError::BranchMissing),
        }
    }

    fn ref_exists(
        &self,
        root: &Path,
        full_name: &str,
        cancellation: &CancellationToken,
    ) -> Result<bool, WorktreeCreateError> {
        // `show-ref --verify --quiet` exits 0 for an existing ref and 1 for a missing one.
        let exit = self
            .git(
                root,
                &["show-ref", "--verify", "--quiet", full_name],
                &mut Vec::new(),
                cancellation,
            )
            .map_err(|_| WorktreeCreateError::Failed)?;
        match exit.code {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(WorktreeCreateError::Failed),
        }
    }

    fn checked_out(
        &self,
        root: &Path,
        branch: &str,
        cancellation: &CancellationToken,
    ) -> Result<bool, WorktreeCreateError> {
        let worktrees = self
            .list_records(root, cancellation)
            .map_err(|_| WorktreeCreateError::Failed)?;
        Ok(worktrees
            .iter()
            .any(|worktree| matches!(&worktree.head, WorktreeHead::Branch(name) if name == branch)))
    }

    fn list_records(
        &self,
        root: &Path,
        cancellation: &CancellationToken,
    ) -> Result<Vec<WorktreeRecord>, RepositoryReadError> {
        let nul = self.git_version >= NUL_LIST_VERSION;
        let mut list = vec!["worktree", "list", "--porcelain"];
        if nul {
            list.push("-z");
        }
        let mut output = Vec::new();
        let exit = self.git(root, &list, &mut output, cancellation)?;
        if !exit.success() {
            return Err(RepositoryReadError::Unavailable);
        }
        parse_worktree_list(&output, if nul { 0 } else { b'\n' })
    }

    fn git(
        &self,
        directory: &Path,
        subcommand: &[&str],
        output: &mut Vec<u8>,
        cancellation: &CancellationToken,
    ) -> Result<ProgramExit, RepositoryReadError> {
        self.run(
            directory,
            git_arguments(directory, FsmonitorPolicy::Disabled, subcommand),
            output,
            cancellation,
        )
    }

    fn run(
        &self,
        directory: &Path,
        arguments: Vec<OsString>,
        output: &mut Vec<u8>,
        cancellation: &CancellationToken,
    ) -> Result<ProgramExit, RepositoryReadError> {
        let request = ProgramRequest {
            executable: self.git.clone(),
            arguments,
            directory: directory.to_path_buf(),
            environment: git_environment(&self.git, &self.physical_home),
            stdout_limit: Some(OUTPUT_LIMIT),
            deadline: None,
            keep_process_group_on_exit: false,
        };
        self.runner
            .run(
                &request,
                &mut |chunk| output.extend_from_slice(chunk),
                cancellation,
            )
            .map_err(repository_program_error)
    }
}

/// Lists modified tracked files and untracked files, one record per path.
const CHANGES_ARGUMENTS: [&str; 5] = [
    "status",
    "--porcelain",
    "-z",
    "--untracked-files=normal",
    "--ignore-submodules=none",
];
const IDENTITY_ARGUMENTS: [&str; 2] = ["rev-parse", "--absolute-git-dir"];
const SUBMODULE_PATH_ARGUMENTS: [&str; 6] = [
    "config",
    "--file",
    ".gitmodules",
    "-z",
    "--get-regexp",
    r"^submodule\..*\.path$",
];

/// Newest first, so the branches people work on lead the pickers.
const BRANCH_ARGUMENTS: [&str; 5] = [
    "for-each-ref",
    "--sort=-committerdate",
    "--format=%(refname)",
    "refs/heads",
    "refs/remotes",
];

/// Parses one full reference name per line. A remote's `HEAD` names no branch of its own.
fn parse_branches(output: &[u8]) -> Result<BranchList, RepositoryReadError> {
    let text = std::str::from_utf8(output).map_err(|_| RepositoryReadError::InvalidResponse)?;
    let mut branches = BranchList::default();
    for line in text.lines().filter(|line| !line.is_empty()) {
        if let Some(name) = line.strip_prefix("refs/heads/") {
            branches.local.push(name.to_owned());
        } else if let Some(name) = line.strip_prefix("refs/remotes/") {
            if !matches!(name.split_once('/'), Some((_, "HEAD")) | None) {
                branches.remote.push(name.to_owned());
            }
        } else {
            return Err(RepositoryReadError::InvalidResponse);
        }
    }
    Ok(branches)
}

fn snapshot(
    worktrees: Vec<WorktreeRecord>,
    current: PathBuf,
    common_directory: PathBuf,
) -> WorktreeSnapshot {
    let main_root = worktrees[0].root.clone();
    WorktreeSnapshot {
        current: worktrees
            .iter()
            .position(|worktree| worktree.root == current),
        main_root,
        common_directory,
        worktrees,
    }
}
