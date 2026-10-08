//! Runs the discovered git for Worktrees, with the same hardening Repository Status reads use.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::listing::{WorktreeRecord, parse_worktree_list};
use super::{WorktreeSnapshot, fixed};
use crate::domain::RepositoryIdentity;
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
}
