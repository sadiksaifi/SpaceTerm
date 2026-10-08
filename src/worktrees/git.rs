//! Runs the discovered git for Worktrees, with the same hardening Repository Status reads use.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use thiserror::Error;

use super::listing::{WorktreeHead, WorktreeRecord, parse_worktree_list};
use super::ref_format::validate_branch_name;
use super::{WorktreeSnapshot, fixed};
use crate::domain::{RepositoryIdentity, ValidatedLocalDirectory};
use crate::platform::local_filesystem::{LocalFilesystemAuthority, LocalFilesystemError};
use crate::repository_status::discovery::{Discovery, parse_discovery};
use crate::repository_status::local_read::{
    DISCOVERY_ARGUMENTS, git_arguments, git_environment, repository_program_error,
};
use crate::repository_status::{
    FsmonitorPolicy, ProgramExit, ProgramRequest, RepositoryProgramRunner, RepositoryReadError,
    ToolVersion,
};
use crate::ssh::cancellation::SshCancellationToken;

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
pub(crate) struct BranchList {
    /// Local branch names, such as `feature/login`.
    pub(crate) local: Vec<String>,
    /// Remote-tracking branch names already fetched, such as `origin/main`.
    pub(crate) remote: Vec<String>,
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
pub(crate) enum WorktreeBranch {
    /// A new branch `name` starting at `base`, a local or remote-tracking branch name. The new
    /// branch tracks nothing.
    New { name: String, base: BranchName },
    /// A local branch no Worktree has checked out.
    Existing { name: String },
    /// A new local branch `name` that tracks the remote-tracking branch `remote`.
    Remote { remote: String, name: String },
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
pub(crate) enum BranchName {
    Local(String),
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
pub(crate) enum WorktreeCreateError {
    #[error("git refuses the branch name")]
    InvalidName,
    #[error("a branch with the name already exists")]
    BranchExists,
    #[error("another Worktree has the branch checked out")]
    BranchCheckedOut,
    #[error("the branch no longer exists")]
    BranchMissing,
    #[error("git could not create the Worktree")]
    Failed,
}

/// Why git did not remove a Worktree.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub(crate) enum WorktreeRemoveError {
    #[error("a different Worktree now has the confirmed location")]
    Replaced,
    #[error("the Worktree could not be checked before removal")]
    Unchecked,
    #[error("git could not remove the Worktree")]
    Failed,
}

/// One Worktree as it was when the person was asked to remove it: git's administrative
/// directory for it and its retained directory. Git names the administrative directory after the
/// Worktree's folder, so a Worktree recreated at the same location can reuse it; the retained
/// directory tells that replacement apart.
#[derive(Clone)]
pub(crate) struct WorktreeIdentity {
    administrative: String,
    directory: ValidatedLocalDirectory,
    filesystem: LocalFilesystemAuthority,
}

impl std::fmt::Debug for WorktreeIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("WorktreeIdentity(..)")
    }
}

/// What the person saw at a Worktree's location when they confirmed its removal. A removal
/// acts only while the location still shows it.
#[derive(Clone)]
pub(crate) enum RemovalExpectation {
    /// The checked Worktree.
    Worktree(WorktreeIdentity),
    /// No directory: git lists the Worktree as missing.
    Absent(LocalFilesystemAuthority),
}

impl std::fmt::Debug for RemovalExpectation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Worktree(_) => "RemovalExpectation::Worktree(..)",
            Self::Absent(_) => "RemovalExpectation::Absent(..)",
        })
    }
}

/// What removing a Worktree would act on, read before the person confirms.
#[derive(Clone, Debug)]
pub(crate) struct RemovalCheck {
    pub(crate) identity: WorktreeIdentity,
    /// Removing it discards modified tracked files or untracked files.
    pub(crate) changes: bool,
}

/// Runs git for one machine's Worktrees. Blocking; callers use a dedicated thread.
pub(crate) struct LocalWorktreeGit {
    runner: Arc<dyn RepositoryProgramRunner>,
    git: PathBuf,
    physical_home: PathBuf,
    git_version: ToolVersion,
}

impl LocalWorktreeGit {
    pub(crate) fn new(
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
    pub(crate) fn list(
        &self,
        directory: &Path,
        cancellation: &SshCancellationToken,
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
        let nul = self.git_version >= NUL_LIST_VERSION;
        let mut list = vec!["worktree", "list", "--porcelain"];
        if nul {
            list.push("-z");
        }
        let mut output = Vec::new();
        let exit = self.run(
            &current,
            git_arguments(&current, FsmonitorPolicy::Disabled, &list),
            &mut output,
            cancellation,
        )?;
        if !exit.success() {
            return Err(RepositoryReadError::Unavailable);
        }
        let worktrees = parse_worktree_list(&output, if nul { 0 } else { b'\n' })?;
        Ok(Some(snapshot(
            worktrees,
            current,
            PathBuf::from(&*repository.common_directory),
        )))
    }

    /// Lists the local and fetched remote-tracking branches of the repository at `root`.
    pub(crate) fn branches(
        &self,
        root: &Path,
        cancellation: &SshCancellationToken,
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
    pub(crate) fn create(
        &self,
        root: &Path,
        path: &Path,
        branch: &WorktreeBranch,
        cancellation: &SshCancellationToken,
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
    pub(crate) fn check_removal(
        &self,
        path: &Path,
        filesystem: &LocalFilesystemAuthority,
        cancellation: &SshCancellationToken,
    ) -> Result<RemovalCheck, WorktreeRemoveError> {
        let directory = filesystem
            .validate_directory(path)
            .map_err(|_| WorktreeRemoveError::Failed)?;
        let identity = WorktreeIdentity {
            administrative: self.administrative_directory(path, cancellation)?,
            directory,
            filesystem: filesystem.clone(),
        };
        let mut output = Vec::new();
        let changes = match self.git(path, &CHANGES_ARGUMENTS, &mut output, cancellation) {
            Ok(exit) if exit.success() => !output.is_empty(),
            // More changes than the output limit holds are still changes.
            Err(RepositoryReadError::OutputTooLarge) => true,
            _ => return Err(WorktreeRemoveError::Failed),
        };
        Ok(RemovalCheck { identity, changes })
    }

    /// Whether the Worktree at `path` is still the one `expected` identifies: the same directory,
    /// which git still administers from the same place.
    pub(crate) fn is_worktree(
        &self,
        path: &Path,
        expected: &WorktreeIdentity,
        cancellation: &SshCancellationToken,
    ) -> Result<(), WorktreeRemoveError> {
        if expected.directory.path() != path {
            return Err(WorktreeRemoveError::Replaced);
        }
        match expected
            .filesystem
            .revalidate_directory(&expected.directory)
        {
            Ok(_) => {}
            Err(LocalFilesystemError::IdentityChanged) => {
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
    pub(crate) fn confirm_location(
        &self,
        path: &Path,
        expected: &RemovalExpectation,
        cancellation: &SshCancellationToken,
    ) -> Result<(), WorktreeRemoveError> {
        match expected {
            RemovalExpectation::Worktree(identity) => {
                self.is_worktree(path, identity, cancellation)
            }
            RemovalExpectation::Absent(filesystem) => match filesystem.validate_directory(path) {
                Err(LocalFilesystemError::Missing) => Ok(()),
                Ok(_) | Err(LocalFilesystemError::NotDirectory) => {
                    Err(WorktreeRemoveError::Replaced)
                }
                Err(_) => Err(WorktreeRemoveError::Failed),
            },
        }
    }

    /// Removes the Worktree at `path` and keeps its branch. Without `force`, git refuses a
    /// Worktree with changes. It first checks that `path` still shows what the person confirmed,
    /// so a confirmation never removes a replacement.
    pub(crate) fn remove(
        &self,
        root: &Path,
        path: &Path,
        force: bool,
        expected: &RemovalExpectation,
        cancellation: &SshCancellationToken,
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
        cancellation: &SshCancellationToken,
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
        cancellation: &SshCancellationToken,
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
        cancellation: &SshCancellationToken,
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
        cancellation: &SshCancellationToken,
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
        cancellation: &SshCancellationToken,
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
        cancellation: &SshCancellationToken,
    ) -> Result<bool, WorktreeCreateError> {
        let nul = self.git_version >= NUL_LIST_VERSION;
        let mut list = vec!["worktree", "list", "--porcelain"];
        if nul {
            list.push("-z");
        }
        let mut output = Vec::new();
        let exit = self
            .git(root, &list, &mut output, cancellation)
            .map_err(|_| WorktreeCreateError::Failed)?;
        if !exit.success() {
            return Err(WorktreeCreateError::Failed);
        }
        let worktrees = parse_worktree_list(&output, if nul { 0 } else { b'\n' })
            .map_err(|_| WorktreeCreateError::Failed)?;
        Ok(worktrees
            .iter()
            .any(|worktree| matches!(&worktree.head, WorktreeHead::Branch(name) if name == branch)))
    }

    fn git(
        &self,
        directory: &Path,
        subcommand: &[&str],
        output: &mut Vec<u8>,
        cancellation: &SshCancellationToken,
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
        cancellation: &SshCancellationToken,
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
const CHANGES_ARGUMENTS: [&str; 4] = ["status", "--porcelain", "-z", "--untracked-files=normal"];
const IDENTITY_ARGUMENTS: [&str; 2] = ["rev-parse", "--absolute-git-dir"];

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
    let repository = RepositoryIdentity::new(worktrees[0].root.clone());
    let current = fixed(&current);
    WorktreeSnapshot {
        current: worktrees
            .iter()
            .position(|worktree| fixed(&worktree.root) == current),
        repository,
        common_directory,
        worktrees,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository_status::ProgramError;
    use crate::repository_status::testing::{FakeRunner, exit};
    use crate::worktrees::listing::WorktreeHead;

    const GIT: &str = "/tools/git/bin/git";
    const HOME: &str = "/Users/person";
    const OID: &str = "1bccb23a55670b76916d70699c1b04ef507bb38d";

    fn git(runner: &Arc<FakeRunner>, minor: u16) -> LocalWorktreeGit {
        LocalWorktreeGit::new(
            Arc::clone(runner) as Arc<dyn RepositoryProgramRunner>,
            GIT.into(),
            HOME.into(),
            ToolVersion {
                major: 2,
                minor,
                patch: 0,
            },
        )
    }

    fn discovery(toplevel: &str, common: &str) -> String {
        format!("false\nfalse\n{common}/worktrees/x\n{common}\n{toplevel}\nsrc/\n")
    }

    #[test]
    fn list_should_run_hardened_git_and_find_the_worktree_containing_the_directory() {
        let runner = FakeRunner::new([
            exit(0, &discovery("/wt/feature", "/src/app/.git")),
            exit(
                0,
                &format!(
                    "worktree /src/app\0HEAD {OID}\0branch refs/heads/main\0\0\
                     worktree /wt/feature\0HEAD {OID}\0branch refs/heads/feature\0\0"
                ),
            ),
        ]);

        let snapshot = git(&runner, 47)
            .list(
                Path::new("/wt/feature/src"),
                &SshCancellationToken::default(),
            )
            .unwrap()
            .unwrap();

        let requests = runner.requests();
        let list = &requests[1];
        assert_eq!(
            list.arguments[list.arguments.len() - 6..],
            ["-C", "/wt/feature", "worktree", "list", "--porcelain", "-z"].map(OsString::from)
        );
        assert!(list.arguments.contains(&"core.hooksPath=/dev/null".into()));
        assert_eq!(
            list.environment,
            git_environment(Path::new(GIT), Path::new(HOME))
        );
        assert_eq!(
            (
                snapshot.current,
                snapshot.repository.main_root(),
                snapshot.common_directory.as_path(),
                snapshot.worktrees[1].head.clone(),
            ),
            (
                Some(1),
                Path::new("/src/app"),
                Path::new("/src/app/.git"),
                WorktreeHead::Branch("feature".into()),
            )
        );
    }

    #[test]
    fn list_should_read_newline_records_from_git_before_2_36() {
        let runner = FakeRunner::new([
            exit(0, &discovery("/src/app", "/src/app/.git")),
            exit(
                0,
                &format!("worktree /src/app\nHEAD {OID}\nbranch refs/heads/main\n\n"),
            ),
        ]);

        let snapshot = git(&runner, 30)
            .list(Path::new("/src/app"), &SshCancellationToken::default())
            .unwrap()
            .unwrap();

        assert_eq!(
            runner.requests()[1].arguments.last(),
            Some(&OsString::from("--porcelain"))
        );
        assert_eq!((snapshot.current, snapshot.worktrees.len()), (Some(0), 1));
    }

    #[test]
    fn list_should_show_nothing_outside_a_work_tree_repository() {
        let runner = FakeRunner::new([exit(128, ""), exit(128, "true\n")]);
        let git = git(&runner, 47);

        let outside = git.list(Path::new("/tmp"), &SshCancellationToken::default());
        let inside_git_directory =
            git.list(Path::new("/src/app/.git"), &SshCancellationToken::default());

        assert_eq!(
            (
                outside.unwrap(),
                inside_git_directory.unwrap(),
                runner.requests().len()
            ),
            (None, None, 2)
        );
    }

    fn tail(request: &ProgramRequest, count: usize) -> Vec<String> {
        request.arguments[request.arguments.len() - count..]
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn branches_should_list_local_and_fetched_remote_branches_without_remote_heads() {
        let runner = FakeRunner::new([exit(
            0,
            "refs/heads/feature/login\nrefs/remotes/origin/HEAD\nrefs/heads/main\n\
             refs/remotes/origin/main\nrefs/remotes/upstream/fix/HEAD\n",
        )]);

        let branches = git(&runner, 47)
            .branches(Path::new("/src/app"), &SshCancellationToken::default())
            .unwrap();

        assert_eq!(
            tail(&runner.requests()[0], 7),
            [
                "-C",
                "/src/app",
                "for-each-ref",
                "--sort=-committerdate",
                "--format=%(refname)",
                "refs/heads",
                "refs/remotes"
            ]
        );
        assert_eq!(
            (branches.local, branches.remote),
            (
                vec!["feature/login".to_owned(), "main".to_owned()],
                vec!["origin/main".to_owned(), "upstream/fix/HEAD".to_owned()]
            )
        );
    }

    #[test]
    fn creating_from_a_new_branch_should_check_the_name_and_base_then_add_without_tracking() {
        let runner = FakeRunner::new([exit(0, ""), exit(1, ""), exit(0, ""), exit(0, "")]);

        let result = git(&runner, 47).create(
            Path::new("/src/app"),
            Path::new("/wt/app/feature-login"),
            &WorktreeBranch::New {
                name: "feature/login".into(),
                base: BranchName::Remote("origin/main".into()),
            },
            &SshCancellationToken::default(),
        );

        let requests = runner.requests();
        assert_eq!(result, Ok(()));
        assert_eq!(
            tail(&requests[0], 3),
            ["check-ref-format", "--branch", "feature/login"]
        );
        assert_eq!(
            tail(&requests[1], 4),
            [
                "show-ref",
                "--verify",
                "--quiet",
                "refs/heads/feature/login"
            ]
        );
        assert_eq!(
            tail(&requests[2], 4),
            [
                "show-ref",
                "--verify",
                "--quiet",
                "refs/remotes/origin/main"
            ]
        );
        assert_eq!(
            tail(&requests[3], 8),
            [
                "worktree",
                "add",
                "--no-track",
                "-b",
                "feature/login",
                "--",
                "/wt/app/feature-login",
                "refs/remotes/origin/main"
            ]
        );
        assert!(
            requests[3]
                .arguments
                .contains(&"core.hooksPath=/dev/null".into())
        );
    }

    #[test]
    fn creating_from_an_existing_branch_should_add_it_by_name_unless_checked_out() {
        let listing = format!(
            "worktree /src/app\0HEAD {OID}\0branch refs/heads/main\0\0\
             worktree /wt/app/fix\0HEAD {OID}\0branch refs/heads/fix\0\0"
        );
        let runner = FakeRunner::new([
            exit(0, ""),
            exit(0, &listing),
            exit(0, ""),
            exit(0, ""),
            exit(0, &listing),
        ]);
        let git = git(&runner, 47);
        let create = |name: &str| {
            git.create(
                Path::new("/src/app"),
                Path::new("/wt/app/x"),
                &WorktreeBranch::Existing { name: name.into() },
                &SshCancellationToken::default(),
            )
        };

        assert_eq!(create("feature"), Ok(()));
        assert_eq!(
            tail(&runner.requests()[2], 5),
            ["worktree", "add", "--", "/wt/app/x", "feature"]
        );
        assert_eq!(create("fix"), Err(WorktreeCreateError::BranchCheckedOut));
        assert_eq!(runner.requests().len(), 5);
    }

    #[test]
    fn creating_from_a_remote_branch_should_track_it_with_a_new_local_branch() {
        let runner = FakeRunner::new([exit(0, ""), exit(1, ""), exit(0, ""), exit(0, "")]);

        let result = git(&runner, 47).create(
            Path::new("/src/app"),
            Path::new("/wt/app/fix"),
            &WorktreeBranch::Remote {
                remote: "origin/fix".into(),
                name: "fix".into(),
            },
            &SshCancellationToken::default(),
        );

        assert_eq!(result, Ok(()));
        assert_eq!(
            tail(&runner.requests()[3], 8),
            [
                "worktree",
                "add",
                "--track",
                "-b",
                "fix",
                "--",
                "/wt/app/fix",
                "refs/remotes/origin/fix"
            ]
        );
    }

    #[test]
    fn create_should_classify_refusals_before_writing() {
        let new = |name: &str| WorktreeBranch::New {
            name: name.into(),
            base: BranchName::Local("main".into()),
        };
        for (branch, responses, error) in [
            (new("bad name"), vec![], WorktreeCreateError::InvalidName),
            (
                new("x"),
                vec![exit(1, "")],
                WorktreeCreateError::InvalidName,
            ),
            (
                new("x"),
                vec![exit(0, ""), exit(0, "")],
                WorktreeCreateError::BranchExists,
            ),
            (
                new("x"),
                vec![exit(0, ""), exit(1, ""), exit(1, "")],
                WorktreeCreateError::BranchMissing,
            ),
            (
                new("x"),
                vec![exit(0, ""), exit(1, ""), exit(0, ""), exit(128, "")],
                WorktreeCreateError::Failed,
            ),
            (
                WorktreeBranch::Existing {
                    name: "gone".into(),
                },
                vec![exit(1, "")],
                WorktreeCreateError::BranchMissing,
            ),
        ] {
            let runner = FakeRunner::new(responses);
            let result = git(&runner, 47).create(
                Path::new("/src/app"),
                Path::new("/wt/app/x"),
                &branch,
                &SshCancellationToken::default(),
            );
            assert_eq!(result, Err(error), "{branch:?}");
        }
    }

    #[test]
    fn changes_should_count_modified_and_untracked_files_and_an_overflowing_listing() {
        const ADMIN: &str = "/src/app/.git/worktrees/x\n";
        let runner = FakeRunner::new([
            exit(0, ADMIN),
            exit(0, ""),
            exit(0, ADMIN),
            exit(0, "?? notes.txt\0"),
            exit(0, ADMIN),
            Err(ProgramError::OutputTooLarge),
            exit(0, ADMIN),
            exit(128, ""),
            exit(128, ""),
        ]);
        let git = git(&runner, 47);
        let filesystem = LocalFilesystemAuthority::testing();
        let location = std::env::temp_dir();
        let changes = || {
            git.check_removal(&location, &filesystem, &SshCancellationToken::default())
                .map(|check| check.changes)
        };

        assert_eq!(
            [changes(), changes(), changes(), changes(), changes()],
            [
                Ok(false),
                Ok(true),
                Ok(true),
                Err(WorktreeRemoveError::Failed),
                Err(WorktreeRemoveError::Failed)
            ]
        );
        let requests = runner.requests();
        assert_eq!(tail(&requests[0], 2), ["rev-parse", "--absolute-git-dir"]);
        assert_eq!(
            tail(&requests[1], 4),
            ["status", "--porcelain", "-z", "--untracked-files=normal"]
        );
    }

    #[test]
    fn remove_should_pass_force_only_when_asked_and_report_failure() {
        let runner = FakeRunner::new([exit(0, ""), exit(0, ""), exit(128, "")]);
        let git = git(&runner, 47);
        let missing = RemovalExpectation::Absent(LocalFilesystemAuthority::testing());
        let remove = |force| {
            git.remove(
                Path::new("/src/app"),
                Path::new("/wt/app/x"),
                force,
                &missing,
                &SshCancellationToken::default(),
            )
        };

        assert_eq!(remove(false), Ok(()));
        assert_eq!(remove(true), Ok(()));
        assert_eq!(remove(true), Err(WorktreeRemoveError::Failed));
        let requests = runner.requests();
        assert_eq!(
            tail(&requests[0], 4),
            ["worktree", "remove", "--", "/wt/app/x"]
        );
        assert_eq!(
            tail(&requests[1], 5),
            ["worktree", "remove", "--force", "--", "/wt/app/x"]
        );
    }

    #[test]
    fn remove_should_refuse_a_different_worktree_at_the_confirmed_location() {
        let runner = FakeRunner::new([
            exit(0, "/src/app/.git/worktrees/x\n"),
            exit(0, "/src/app/.git/worktrees/x\n"),
            exit(0, "/src/app/.git/worktrees/x1\n"),
            exit(0, "/src/app/.git/worktrees/x\n"),
            exit(0, ""),
        ]);
        let git = git(&runner, 47);
        let cancellation = SshCancellationToken::default();
        let location = std::env::temp_dir();
        let path = location.as_path();
        let confirmed = git
            .check_removal(path, &LocalFilesystemAuthority::testing(), &cancellation)
            .unwrap()
            .identity;
        let remove = || {
            git.remove(
                Path::new("/src/app"),
                path,
                true,
                &RemovalExpectation::Worktree(confirmed.clone()),
                &cancellation,
            )
        };

        assert_eq!(remove(), Err(WorktreeRemoveError::Replaced));
        assert_eq!(remove(), Ok(()));
        let requests = runner.requests();
        assert_eq!(requests.len(), 5, "a replacement is never removed");
        assert_eq!(
            tail(&requests[4], 5),
            [
                "worktree",
                "remove",
                "--force",
                "--",
                path.to_str().unwrap()
            ]
        );
        assert_eq!(format!("{confirmed:?}"), "WorktreeIdentity(..)");
    }

    #[test]
    fn remove_should_not_run_git_once_the_confirmed_directory_is_gone() {
        let location =
            std::env::temp_dir().join(format!("spaceterm-worktree-gone-{}", std::process::id()));
        std::fs::create_dir_all(&location).unwrap();
        let runner = FakeRunner::new([exit(0, "/src/app/.git/worktrees/x\n"), exit(0, "")]);
        let git = git(&runner, 47);
        let cancellation = SshCancellationToken::default();
        let confirmed = git
            .check_removal(
                &location,
                &LocalFilesystemAuthority::testing(),
                &cancellation,
            )
            .unwrap()
            .identity;
        std::fs::remove_dir(&location).unwrap();

        assert_eq!(
            git.remove(
                Path::new("/src/app"),
                &location,
                true,
                &RemovalExpectation::Worktree(confirmed),
                &cancellation,
            ),
            Err(WorktreeRemoveError::Failed)
        );
        assert_eq!(runner.requests().len(), 2, "git never runs a removal");
    }

    #[test]
    fn remove_should_refuse_a_directory_where_a_missing_worktree_was_confirmed() {
        let runner = FakeRunner::new([]);
        let git = git(&runner, 47);
        let location = std::env::temp_dir();

        assert_eq!(
            git.remove(
                Path::new("/src/app"),
                &location,
                true,
                &RemovalExpectation::Absent(LocalFilesystemAuthority::testing()),
                &SshCancellationToken::default(),
            ),
            Err(WorktreeRemoveError::Replaced)
        );
        assert!(runner.requests().is_empty(), "git never runs a removal");
    }
}
