//! Desktop compatibility adapter for host-owned Worktree execution.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use host::filesystem::{DirectoryError, RetainedDirectory, WorktreeFilesystem};
use spaceterm_host::git as host;

use super::WorktreeSnapshot;
use crate::domain::{RepositoryIdentity, ValidatedLocalDirectory};
use crate::platform::local_filesystem::{LocalFilesystemAuthority, LocalFilesystemError};
use crate::repository_status::{RepositoryProgramRunner, RepositoryReadError, ToolVersion};
use crate::ssh::cancellation::SshCancellationToken;

pub(crate) use host::{
    BranchList, BranchName, RemovalCheck, WorktreeBranch, WorktreeCreateError, WorktreeIdentity,
    WorktreeRemoveError,
};

/// Desktop confirmations retain the existing Local Filesystem Authority.
#[derive(Clone)]
pub(crate) enum RemovalExpectation {
    Worktree {
        identity: WorktreeIdentity,
        require_clean: bool,
    },
    Absent(LocalFilesystemAuthority),
}

impl std::fmt::Debug for RemovalExpectation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Worktree { .. } => "RemovalExpectation::Worktree(..)",
            Self::Absent(_) => "RemovalExpectation::Absent(..)",
        })
    }
}

impl RemovalExpectation {
    fn host(&self) -> host::RemovalExpectation {
        match self {
            Self::Worktree {
                identity,
                require_clean,
            } => host::RemovalExpectation::Worktree {
                identity: identity.clone(),
                require_clean: *require_clean,
            },
            Self::Absent(filesystem) => {
                host::RemovalExpectation::Absent(Arc::new(DesktopFilesystem(filesystem.clone())))
            }
        }
    }
}

struct DesktopFilesystem(LocalFilesystemAuthority);

impl WorktreeFilesystem for DesktopFilesystem {
    fn validate_directory(
        &self,
        path: &Path,
    ) -> Result<Arc<dyn RetainedDirectory>, DirectoryError> {
        let directory = self.0.validate_directory(path).map_err(directory_error)?;
        Ok(Arc::new(DesktopDirectory {
            directory,
            filesystem: self.0.clone(),
        }))
    }

    fn has_entry(&self, path: &Path) -> bool {
        self.0.has_entry(path)
    }
}

struct DesktopDirectory {
    directory: ValidatedLocalDirectory,
    filesystem: LocalFilesystemAuthority,
}

impl RetainedDirectory for DesktopDirectory {
    fn path(&self) -> &Path {
        self.directory.path()
    }

    fn revalidate(&self) -> Result<(), DirectoryError> {
        self.filesystem
            .revalidate_directory(&self.directory)
            .map(drop)
            .map_err(directory_error)
    }
}

fn directory_error(error: LocalFilesystemError) -> DirectoryError {
    match error {
        LocalFilesystemError::Missing => DirectoryError::Missing,
        LocalFilesystemError::NotDirectory => DirectoryError::NotDirectory,
        LocalFilesystemError::IdentityChanged => DirectoryError::IdentityChanged,
        LocalFilesystemError::Capacity => DirectoryError::Capacity,
        _ => DirectoryError::Unavailable,
    }
}

/// Runs host policy while retaining desktop filesystem authority and snapshot identities.
pub(crate) struct LocalWorktreeGit(host::LocalWorktreeGit);

impl LocalWorktreeGit {
    pub(crate) fn new(
        runner: Arc<dyn RepositoryProgramRunner>,
        git: PathBuf,
        physical_home: PathBuf,
        git_version: ToolVersion,
    ) -> Self {
        Self(host::LocalWorktreeGit::new(
            runner,
            git,
            physical_home,
            git_version,
        ))
    }

    pub(crate) fn list(
        &self,
        directory: &Path,
        cancellation: &SshCancellationToken,
    ) -> Result<Option<WorktreeSnapshot>, RepositoryReadError> {
        self.0.list(directory, cancellation).map(|snapshot| {
            snapshot.map(|snapshot| WorktreeSnapshot {
                repository: RepositoryIdentity::new(snapshot.main_root),
                current: snapshot.current,
                common_directory: snapshot.common_directory,
                worktrees: snapshot.worktrees,
            })
        })
    }

    pub(crate) fn branches(
        &self,
        root: &Path,
        cancellation: &SshCancellationToken,
    ) -> Result<BranchList, RepositoryReadError> {
        self.0.branches(root, cancellation)
    }

    pub(crate) fn create(
        &self,
        root: &Path,
        path: &Path,
        branch: &WorktreeBranch,
        cancellation: &SshCancellationToken,
    ) -> Result<(), WorktreeCreateError> {
        self.0.create(root, path, branch, cancellation)
    }

    pub(crate) fn check_removal(
        &self,
        path: &Path,
        filesystem: &LocalFilesystemAuthority,
        cancellation: &SshCancellationToken,
    ) -> Result<RemovalCheck, WorktreeRemoveError> {
        let filesystem: Arc<dyn WorktreeFilesystem> =
            Arc::new(DesktopFilesystem(filesystem.clone()));
        self.0.check_removal(path, &filesystem, cancellation)
    }

    pub(crate) fn confirm_location(
        &self,
        path: &Path,
        expected: &RemovalExpectation,
        cancellation: &SshCancellationToken,
    ) -> Result<(), WorktreeRemoveError> {
        self.0
            .confirm_location(path, &expected.host(), cancellation)
    }

    pub(crate) fn remove(
        &self,
        root: &Path,
        path: &Path,
        force: bool,
        expected: &RemovalExpectation,
        cancellation: &SshCancellationToken,
    ) -> Result<(), WorktreeRemoveError> {
        self.0
            .remove(root, path, force, &expected.host(), cancellation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository_status::testing::{FakeRunner, exit};
    use crate::repository_status::{ProgramError, ProgramRequest};
    use crate::worktrees::listing::WorktreeHead;
    use host::local_read::git_environment;
    use std::ffi::OsString;

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
        // Each checked Worktree also reads `.gitmodules`, which these lack.
        let runner = FakeRunner::new([
            exit(0, ADMIN),
            exit(0, ""),
            exit(1, ""),
            exit(0, ADMIN),
            exit(0, "?? notes.txt\0"),
            exit(1, ""),
            exit(0, ADMIN),
            Err(ProgramError::OutputTooLarge),
            exit(1, ""),
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
            tail(&requests[1], 5),
            [
                "status",
                "--porcelain",
                "-z",
                "--untracked-files=normal",
                "--ignore-submodules=none"
            ]
        );
    }

    #[test]
    fn a_checked_out_submodule_should_need_force_however_git_keeps_it() {
        let root = std::env::temp_dir().join(format!(
            "spaceterm-worktree-submodules-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let worktree = root.join("wt");
        let administrative = root.join("admin");
        std::fs::create_dir_all(worktree.join("lib/.git")).unwrap();
        std::fs::create_dir_all(administrative.join("modules")).unwrap();
        let admin = format!("{}\n", administrative.display());
        let elsewhere = format!("{}\n", root.join("other").display());
        let runner = FakeRunner::new([
            // Its administrative directory keeps the submodule's repository.
            exit(0, &admin),
            exit(0, ""),
            // `.gitmodules` names a path with a `.git`.
            exit(0, &elsewhere),
            exit(0, ""),
            exit(0, "submodule.lib.path\nlib\0"),
            // `.gitmodules` names only paths without a `.git`, or escaping the Worktree.
            exit(0, &elsewhere),
            exit(0, ""),
            exit(
                0,
                "submodule.a.path\nmissing\0submodule.b.path\n../wt/lib\0",
            ),
        ]);
        let git = git(&runner, 47);
        let filesystem = LocalFilesystemAuthority::testing();
        let submodules = || {
            git.check_removal(&worktree, &filesystem, &SshCancellationToken::default())
                .map(|check| (check.changes, check.submodules))
        };

        let checks = [submodules(), submodules(), submodules()];
        let _ = std::fs::remove_dir_all(&root);

        assert_eq!(
            checks,
            [Ok((false, true)), Ok((false, true)), Ok((false, false))]
        );
        assert_eq!(
            tail(&runner.requests()[4], 6),
            [
                "config",
                "--file",
                ".gitmodules",
                "-z",
                "--get-regexp",
                r"^submodule\..*\.path$"
            ]
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
            exit(0, ""),
            exit(1, ""),
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
                &RemovalExpectation::Worktree {
                    identity: confirmed.clone(),
                    require_clean: false,
                },
                &cancellation,
            )
        };

        assert_eq!(remove(), Err(WorktreeRemoveError::Replaced));
        assert_eq!(remove(), Ok(()));
        let requests = runner.requests();
        assert_eq!(requests.len(), 6, "a replacement is never removed");
        assert_eq!(
            tail(&requests[5], 5),
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
        let runner = FakeRunner::new([
            exit(0, "/src/app/.git/worktrees/x\n"),
            exit(0, ""),
            exit(1, ""),
        ]);
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
                &RemovalExpectation::Worktree {
                    identity: confirmed,
                    require_clean: false,
                },
                &cancellation,
            ),
            Err(WorktreeRemoveError::Failed)
        );
        assert_eq!(runner.requests().len(), 3, "git never runs a removal");
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
