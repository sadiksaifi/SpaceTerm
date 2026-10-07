//! The local read policy: which git commands a local probe and count run, with what hardening,
//! and how their output becomes Repository Status facts.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::discovery::{
    ConfigParser, Discovery, parse_discovery, repository_head, short_commit, upstream,
};
use super::operation::repository_operation;
use super::porcelain::PorcelainParser;
use super::{
    FsmonitorPolicy, PorcelainSummary, ProbeOutcome, ProbedRepository, ProgramError, ProgramExit,
    ProgramRequest, RepositoryMarkerReader, RepositoryProgramRunner, RepositoryReadError,
    RepositoryRoot, StatusHeaders, ToolVersion, tool_search_path,
};
use crate::ssh::cancellation::SshCancellationToken;

pub(crate) const DISCOVERY_ARGUMENTS: [&str; 7] = [
    "rev-parse",
    "--is-inside-git-dir",
    "--is-bare-repository",
    "--absolute-git-dir",
    "--git-common-dir",
    "--show-toplevel",
    "--show-prefix",
];
/// Headers only: the exclude-everything pathspec, anchored at the top level, keeps git from
/// reporting any work tree entry.
pub(crate) const HEADER_ARGUMENTS: [&str; 8] = [
    "status",
    "--porcelain=v2",
    "--branch",
    "-z",
    "--untracked-files=no",
    "--ignore-submodules=all",
    "--",
    ":(top,exclude)*",
];
pub(crate) const CONFIG_ARGUMENTS: [&str; 4] = [
    "config",
    "-z",
    "--get-regexp",
    r"^(core\.fsmonitor|push\.default|remote\.pushdefault|remote\..*\.url|branch\..*\.(remote|merge|pushremote))$",
];
/// No untracked flag, so `status.showUntrackedFiles` applies.
pub(crate) const COUNT_ARGUMENTS: [&str; 4] = ["status", "--porcelain=v2", "--branch", "-z"];

/// Small probe outputs: three short paths, or a handful of header records.
const PROBE_OUTPUT_LIMIT: usize = 64 * 1024;
/// `git config` exits 1 when no key matches.
const CONFIG_NO_MATCH_STATUS: i32 = 1;

/// Hardened git arguments: no optional locks, pager, hooks, transports, color, quoting, or
/// hook-program fsmonitor, run in `directory`.
pub(crate) fn git_arguments(
    directory: &Path,
    fsmonitor: FsmonitorPolicy,
    subcommand: &[&str],
) -> Vec<OsString> {
    let mut arguments: Vec<OsString> = vec!["--no-optional-locks".into(), "--no-pager".into()];
    let mut configuration = vec![
        "core.hooksPath=/dev/null",
        // Git before 2.44 ignores `GIT_NO_LAZY_FETCH`; with no allowed transport, a partial
        // clone's missing object fails the read instead of fetching.
        "protocol.allow=never",
        "color.ui=false",
        "core.quotePath=false",
        "status.relativePaths=false",
        "advice.statusHints=false",
    ];
    // Pinning the probed policy keeps a value changed since the probe, such as a hook program,
    // from taking effect.
    configuration.insert(
        0,
        match fsmonitor {
            FsmonitorPolicy::Disabled => "core.fsmonitor=false",
            FsmonitorPolicy::Builtin => "core.fsmonitor=true",
        },
    );
    for setting in configuration {
        arguments.extend(["-c".into(), setting.into()]);
    }
    arguments.extend(["-C".into(), directory.as_os_str().to_owned()]);
    arguments.extend(subcommand.iter().map(OsString::from));
    arguments
}

/// Arguments for the configuration read, which carries no `-c` settings: `git config` lists
/// command-line settings as configuration, so any override would hide the repository's own value.
/// Reading configuration runs no hooks and no fsmonitor.
pub(crate) fn config_arguments(directory: &Path) -> Vec<OsString> {
    let mut arguments: Vec<OsString> = vec![
        "--no-optional-locks".into(),
        "--no-pager".into(),
        "-C".into(),
        directory.as_os_str().to_owned(),
    ];
    arguments.extend(CONFIG_ARGUMENTS.iter().map(OsString::from));
    arguments
}

/// The complete git environment. Nothing is inherited.
pub(crate) fn git_environment(executable: &Path, home: &Path) -> Vec<(OsString, OsString)> {
    let mut environment: Vec<(OsString, OsString)> = vec![
        ("HOME".into(), home.as_os_str().to_owned()),
        ("PATH".into(), tool_search_path(executable)),
    ];
    environment.extend(
        [
            ("LC_ALL", "C"),
            ("GIT_TERMINAL_PROMPT", "0"),
            ("GIT_OPTIONAL_LOCKS", "0"),
            ("GIT_PAGER", "cat"),
            ("PAGER", "cat"),
            ("GIT_NO_LAZY_FETCH", "1"),
        ]
        .map(|(name, value)| (name.into(), value.into())),
    );
    environment
}

pub(crate) fn repository_program_error(error: ProgramError) -> RepositoryReadError {
    match error {
        ProgramError::NotFound => RepositoryReadError::ToolMissing,
        ProgramError::Cancelled => RepositoryReadError::Cancelled,
        ProgramError::TimedOut => RepositoryReadError::TimedOut,
        ProgramError::OutputTooLarge => RepositoryReadError::OutputTooLarge,
        ProgramError::Failed => RepositoryReadError::Unavailable,
    }
}

/// Reads local repositories by running the discovered git. Blocking; callers use a dedicated
/// thread.
pub(crate) struct LocalRepositoryReader {
    runner: Arc<dyn RepositoryProgramRunner>,
    markers: Arc<dyn RepositoryMarkerReader>,
    git: PathBuf,
    /// The account's home with symbolic links resolved, captured at composition.
    physical_home: PathBuf,
    git_version: ToolVersion,
}

impl fmt::Debug for LocalRepositoryReader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LocalRepositoryReader")
            .field("git_version", &self.git_version)
            .finish_non_exhaustive()
    }
}

impl LocalRepositoryReader {
    pub(crate) fn new(
        runner: Arc<dyn RepositoryProgramRunner>,
        markers: Arc<dyn RepositoryMarkerReader>,
        git: PathBuf,
        physical_home: PathBuf,
        git_version: ToolVersion,
    ) -> Self {
        Self {
            runner,
            markers,
            git,
            physical_home,
            git_version,
        }
    }

    /// Finds the repository containing `directory` and reads everything except change counts.
    /// It never walks the work tree.
    pub(crate) fn probe(
        &self,
        directory: &Path,
        cancellation: &SshCancellationToken,
    ) -> Result<ProbeOutcome, RepositoryReadError> {
        // Later commands name the paths git prints, so they must round-trip through text.
        let directory_text = directory.to_str().ok_or(RepositoryReadError::Unavailable)?;
        let mut output = Vec::new();
        let exit = self.run(
            self.request(
                directory,
                git_arguments(directory, FsmonitorPolicy::Disabled, &DISCOVERY_ARGUMENTS),
                Some(PROBE_OUTPUT_LIMIT),
            ),
            &mut |chunk| output.extend_from_slice(chunk),
            cancellation,
        )?;
        let repository = match parse_discovery(
            &output,
            directory_text,
            &self.physical_home.to_string_lossy(),
        )? {
            Discovery::NotRepository => return Ok(ProbeOutcome::NotRepository),
            Discovery::Hidden => return Ok(ProbeOutcome::Hidden),
            Discovery::Repository(repository) => repository,
        };
        if !exit.success() {
            return Err(RepositoryReadError::Unavailable);
        }
        let root = PathBuf::from(&*repository.toplevel);
        let headers = self.headers(&root, cancellation)?;
        let head = repository_head(&headers)?;
        let config = self.config(&root, headers.branch.as_deref(), cancellation)?;
        let git_directory = PathBuf::from(&*repository.git_directory);
        let operation = repository_operation(&self.markers.read(&git_directory));
        Ok(ProbeOutcome::Repository(Box::new(ProbedRepository {
            root: RepositoryRoot::Local(root),
            git_directory: RepositoryRoot::Local(git_directory),
            common_directory: RepositoryRoot::Local(PathBuf::from(&*repository.common_directory)),
            commit: short_commit(&headers),
            upstream: upstream(&headers),
            head,
            operation,
            config,
        })))
    }

    /// Counts changes below `root`, streaming git's output into the parser without a size limit.
    /// With the built-in fsmonitor, its daemon outlives the run.
    pub(crate) fn count(
        &self,
        root: &Path,
        fsmonitor: FsmonitorPolicy,
        cancellation: &SshCancellationToken,
    ) -> Result<PorcelainSummary, RepositoryReadError> {
        let mut request =
            self.request(root, git_arguments(root, fsmonitor, &COUNT_ARGUMENTS), None);
        request.keep_process_group_on_exit = fsmonitor == FsmonitorPolicy::Builtin;
        let mut parser = PorcelainParser::new();
        let exit = self.run(request, &mut |chunk| parser.push(chunk), cancellation)?;
        if !exit.success() {
            return Err(RepositoryReadError::Unavailable);
        }
        parser.finish()
    }

    fn headers(
        &self,
        root: &Path,
        cancellation: &SshCancellationToken,
    ) -> Result<StatusHeaders, RepositoryReadError> {
        let mut parser = PorcelainParser::new();
        let exit = self.run(
            self.request(
                root,
                git_arguments(root, FsmonitorPolicy::Disabled, &HEADER_ARGUMENTS),
                Some(PROBE_OUTPUT_LIMIT),
            ),
            &mut |chunk| parser.push(chunk),
            cancellation,
        )?;
        if !exit.success() {
            return Err(RepositoryReadError::Unavailable);
        }
        Ok(parser.finish()?.headers)
    }

    fn config(
        &self,
        root: &Path,
        branch: Option<&str>,
        cancellation: &SshCancellationToken,
    ) -> Result<super::RepositoryConfig, RepositoryReadError> {
        let mut parser = ConfigParser::new(branch, self.git_version);
        let exit = self.run(
            self.request(root, config_arguments(root), None),
            &mut |chunk| parser.push(chunk),
            cancellation,
        )?;
        if !matches!(exit.code, Some(0 | CONFIG_NO_MATCH_STATUS)) {
            return Err(RepositoryReadError::Unavailable);
        }
        parser.finish()
    }

    fn request(
        &self,
        directory: &Path,
        arguments: Vec<OsString>,
        stdout_limit: Option<usize>,
    ) -> ProgramRequest {
        ProgramRequest {
            executable: self.git.clone(),
            arguments,
            directory: directory.to_path_buf(),
            environment: git_environment(&self.git, &self.physical_home),
            stdout_limit,
            deadline: None,
            keep_process_group_on_exit: false,
        }
    }

    fn run(
        &self,
        request: ProgramRequest,
        stdout: &mut dyn FnMut(&[u8]),
        cancellation: &SshCancellationToken,
    ) -> Result<ProgramExit, RepositoryReadError> {
        self.runner
            .run(&request, stdout, cancellation)
            .map_err(repository_program_error)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::repository_status::testing::{FakeRunner, exit};
    use crate::repository_status::{
        Divergence, OperationMarkers, OperationStep, RepositoryConfig, RepositoryHead,
        RepositoryOperation, StepMarkers, Upstream,
    };

    const GIT: &str = "/tools/git/bin/git";
    const HOME: &str = "/Users/person";
    const VERSION: ToolVersion = ToolVersion {
        major: 2,
        minor: 47,
        patch: 0,
    };
    const OID: &str = "1bccb23a55670b76916d70699c1b04ef507bb38d";

    #[derive(Default)]
    struct FakeMarkers {
        markers: OperationMarkers,
        reads: Mutex<Vec<PathBuf>>,
    }

    impl RepositoryMarkerReader for FakeMarkers {
        fn read(&self, git_directory: &Path) -> OperationMarkers {
            self.reads.lock().unwrap().push(git_directory.to_path_buf());
            self.markers.clone()
        }
    }

    fn reader(runner: &Arc<FakeRunner>, markers: &Arc<FakeMarkers>) -> LocalRepositoryReader {
        LocalRepositoryReader::new(
            Arc::clone(runner) as Arc<dyn RepositoryProgramRunner>,
            Arc::clone(markers) as Arc<dyn RepositoryMarkerReader>,
            PathBuf::from(GIT),
            PathBuf::from(HOME),
            VERSION,
        )
    }

    fn strings(arguments: &[OsString]) -> Vec<&str> {
        arguments
            .iter()
            .map(|argument| argument.to_str().unwrap())
            .collect()
    }

    fn hardened(fsmonitor_disabled: bool, directory: &str, subcommand: &[&str]) -> Vec<String> {
        let mut expected = vec!["--no-optional-locks", "--no-pager", "-c"];
        expected.push(if fsmonitor_disabled {
            "core.fsmonitor=false"
        } else {
            "core.fsmonitor=true"
        });
        expected.extend([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "protocol.allow=never",
            "-c",
            "color.ui=false",
            "-c",
            "core.quotePath=false",
            "-c",
            "status.relativePaths=false",
            "-c",
            "advice.statusHints=false",
            "-C",
            directory,
        ]);
        expected.extend(subcommand);
        expected.into_iter().map(str::to_owned).collect()
    }

    fn expected_environment() -> Vec<(OsString, OsString)> {
        [
            ("HOME", HOME),
            ("PATH", "/tools/git/bin:/usr/bin:/bin"),
            ("LC_ALL", "C"),
            ("GIT_TERMINAL_PROMPT", "0"),
            ("GIT_OPTIONAL_LOCKS", "0"),
            ("GIT_PAGER", "cat"),
            ("PAGER", "cat"),
            ("GIT_NO_LAZY_FETCH", "1"),
        ]
        .map(|(name, value)| (name.into(), value.into()))
        .into()
    }

    fn assert_git_request(
        request: &ProgramRequest,
        directory: &str,
        fsmonitor_disabled: bool,
        subcommand: &[&str],
        stdout_limit: Option<usize>,
        keep_process_group_on_exit: bool,
    ) {
        assert_eq!(request.executable, Path::new(GIT));
        assert_eq!(
            strings(&request.arguments),
            hardened(fsmonitor_disabled, directory, subcommand)
        );
        assert_eq!(request.directory, Path::new(directory));
        assert_eq!(request.environment, expected_environment());
        assert_eq!(request.stdout_limit, stdout_limit);
        assert_eq!(request.deadline, None);
        assert_eq!(
            request.keep_process_group_on_exit,
            keep_process_group_on_exit
        );
    }

    fn discovery(prefix: &str) -> String {
        format!("false\nfalse\n/src/app/.git\n../.git\n/src/app\n{prefix}\n")
    }

    fn headers() -> String {
        format!(
            "# branch.oid {OID}\0# branch.head main\0# branch.upstream origin/main\0\
             # branch.ab +1 -2\0"
        )
    }

    #[test]
    fn probe_should_run_hardened_discovery_headers_and_config_then_read_markers() {
        let runner = FakeRunner::new([
            exit(0, &discovery("src/")),
            exit(0, &headers()),
            exit(
                0,
                "core.fsmonitor\ntrue\0remote.origin.url\ngit@github.com:me/app.git\0\
                 branch.main.remote\norigin\0branch.main.merge\nrefs/heads/main\0",
            ),
        ]);
        let markers = Arc::new(FakeMarkers {
            markers: OperationMarkers {
                rebase_merge: Some(StepMarkers {
                    current: Some(b"2\n".to_vec()),
                    total: Some(b"5\n".to_vec()),
                }),
                ..OperationMarkers::default()
            },
            ..FakeMarkers::default()
        });

        let outcome = reader(&runner, &markers)
            .probe(Path::new("/src/app/src"), &SshCancellationToken::default())
            .unwrap();

        assert_eq!(
            outcome,
            ProbeOutcome::Repository(Box::new(ProbedRepository {
                root: RepositoryRoot::Local("/src/app".into()),
                git_directory: RepositoryRoot::Local("/src/app/.git".into()),
                common_directory: RepositoryRoot::Local("/src/app/src/../.git".into()),
                head: RepositoryHead::Branch("main".into()),
                commit: Some("1bccb23".into()),
                upstream: Some(Upstream {
                    name: "origin/main".into(),
                    divergence: Some(Divergence {
                        ahead: 1,
                        behind: 2
                    }),
                }),
                operation: Some(RepositoryOperation::Rebasing {
                    step: Some(OperationStep {
                        current: 2,
                        total: 5
                    })
                }),
                config: RepositoryConfig {
                    fsmonitor: FsmonitorPolicy::Builtin,
                    remotes: vec![("origin".into(), "git@github.com:me/app.git".into())],
                    push_default: None,
                    push_to_upstream: false,
                    branch_remote: Some("origin".into()),
                    branch_merge: Some("refs/heads/main".into()),
                    branch_push_remote: None,
                },
            }))
        );
        let requests = runner.requests();
        assert_eq!(requests.len(), 3);
        assert_git_request(
            &requests[0],
            "/src/app/src",
            true,
            &DISCOVERY_ARGUMENTS,
            Some(PROBE_OUTPUT_LIMIT),
            false,
        );
        assert_git_request(
            &requests[1],
            "/src/app",
            true,
            &HEADER_ARGUMENTS,
            Some(PROBE_OUTPUT_LIMIT),
            false,
        );
        assert_eq!(
            strings(&requests[2].arguments),
            [
                "--no-optional-locks",
                "--no-pager",
                "-C",
                "/src/app",
                "config",
                "-z",
                "--get-regexp",
                CONFIG_ARGUMENTS[3],
            ]
        );
        assert_eq!(requests[2].directory, Path::new("/src/app"));
        assert_eq!(requests[2].environment, expected_environment());
        assert_eq!(requests[2].stdout_limit, None);
        assert!(!requests[2].keep_process_group_on_exit);
        assert_eq!(
            *markers.reads.lock().unwrap(),
            [PathBuf::from("/src/app/.git")]
        );
    }

    #[test]
    fn probe_arguments_should_match_the_contract_spelling() {
        assert_eq!(
            CONFIG_ARGUMENTS[3],
            "^(core\\.fsmonitor|push\\.default|remote\\.pushdefault|remote\\..*\\.url|branch\\..*\\.(remote|merge|pushremote))$"
        );
        assert_eq!(HEADER_ARGUMENTS[6..], ["--", ":(top,exclude)*"]);
    }

    #[test]
    fn header_and_config_reads_should_use_the_exact_corrected_arguments() {
        let runner = FakeRunner::new([
            exit(0, &discovery("src/")),
            exit(0, &headers()),
            exit(0, "core.fsmonitor\ntrue\0"),
        ]);

        let outcome = reader(&runner, &Arc::default())
            .probe(Path::new("/src/app/src"), &SshCancellationToken::default())
            .unwrap();

        let requests = runner.requests();
        assert_eq!(
            strings(&requests[1].arguments),
            [
                "--no-optional-locks",
                "--no-pager",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "protocol.allow=never",
                "-c",
                "color.ui=false",
                "-c",
                "core.quotePath=false",
                "-c",
                "status.relativePaths=false",
                "-c",
                "advice.statusHints=false",
                "-C",
                "/src/app",
                "status",
                "--porcelain=v2",
                "--branch",
                "-z",
                "--untracked-files=no",
                "--ignore-submodules=all",
                "--",
                ":(top,exclude)*",
            ]
        );
        // No `-c` setting reaches `git config`, so the repository's own fsmonitor value survives.
        assert_eq!(
            strings(&requests[2].arguments),
            [
                "--no-optional-locks",
                "--no-pager",
                "-C",
                "/src/app",
                "config",
                "-z",
                "--get-regexp",
                "^(core\\.fsmonitor|push\\.default|remote\\.pushdefault|remote\\..*\\.url|branch\\..*\\.(remote|merge|pushremote))$",
            ]
        );
        let ProbeOutcome::Repository(repository) = outcome else {
            panic!("expected a repository");
        };
        assert_eq!(repository.config.fsmonitor, FsmonitorPolicy::Builtin);
    }

    #[test]
    fn probe_should_stop_after_discovery_outside_a_repository() {
        let runner = FakeRunner::new([exit(128, "")]);
        let markers = Arc::new(FakeMarkers::default());

        let outcome = reader(&runner, &markers)
            .probe(Path::new("/tmp"), &SshCancellationToken::default())
            .unwrap();

        assert_eq!(outcome, ProbeOutcome::NotRepository);
        assert_eq!(runner.requests().len(), 1);
        assert!(markers.reads.lock().unwrap().is_empty());
    }

    #[test]
    fn probe_should_hide_git_directories_bare_repositories_and_home_subdirectories() {
        let home_below = format!("false\nfalse\n{HOME}/.git\n../.git\n{HOME}\nProjects/\n");
        for output in [
            "true\nfalse\n/src/app/.git\n.\n",
            "true\ntrue\n/src/bare.git\n.\n",
            home_below.as_str(),
        ] {
            let code = if output.starts_with("true") { 128 } else { 0 };
            let runner = FakeRunner::new([exit(code, output)]);
            let markers = Arc::new(FakeMarkers::default());

            let outcome = reader(&runner, &markers)
                .probe(Path::new("/src/x"), &SshCancellationToken::default())
                .unwrap();

            assert_eq!(outcome, ProbeOutcome::Hidden, "{output:?}");
            assert_eq!(runner.requests().len(), 1);
        }
    }

    #[test]
    fn probe_should_treat_a_failed_discovery_with_repository_output_as_unavailable() {
        let runner = FakeRunner::new([exit(128, &discovery(""))]);

        let result = reader(&runner, &Arc::default())
            .probe(Path::new("/src/app"), &SshCancellationToken::default());

        assert_eq!(result, Err(RepositoryReadError::Unavailable));
    }

    #[test]
    fn probe_should_report_failed_header_and_config_reads() {
        for (responses, expected) in [
            (
                vec![exit(0, &discovery("")), exit(128, "")],
                RepositoryReadError::Unavailable,
            ),
            (
                vec![exit(0, &discovery("")), exit(0, "garbage\0")],
                RepositoryReadError::InvalidResponse,
            ),
            (
                vec![exit(0, &discovery("")), exit(0, &headers()), exit(2, "")],
                RepositoryReadError::Unavailable,
            ),
            (
                vec![exit(0, "false\nfalse\n")],
                RepositoryReadError::InvalidResponse,
            ),
        ] {
            let runner = FakeRunner::new(responses);

            let result = reader(&runner, &Arc::default())
                .probe(Path::new("/src/app"), &SshCancellationToken::default());

            assert_eq!(result, Err(expected));
        }
    }

    #[test]
    fn probe_should_accept_config_without_matches() {
        let runner = FakeRunner::new([
            exit(0, &discovery("")),
            exit(0, "# branch.oid (initial)\0# branch.head trunk\0"),
            exit(1, ""),
        ]);

        let ProbeOutcome::Repository(repository) = reader(&runner, &Arc::default())
            .probe(Path::new("/src/app"), &SshCancellationToken::default())
            .unwrap()
        else {
            panic!("expected a repository");
        };

        assert_eq!(repository.head, RepositoryHead::Unborn("trunk".into()));
        assert_eq!(repository.commit, None);
        assert_eq!(repository.upstream, None);
        assert_eq!(repository.operation, None);
        assert_eq!(repository.config, RepositoryConfig::default());
    }

    #[test]
    fn program_errors_should_map_to_read_errors() {
        for (error, expected) in [
            (ProgramError::NotFound, RepositoryReadError::ToolMissing),
            (ProgramError::Cancelled, RepositoryReadError::Cancelled),
            (ProgramError::TimedOut, RepositoryReadError::TimedOut),
            (
                ProgramError::OutputTooLarge,
                RepositoryReadError::OutputTooLarge,
            ),
            (ProgramError::Failed, RepositoryReadError::Unavailable),
        ] {
            let runner = FakeRunner::new([Err(error)]);

            let result = reader(&runner, &Arc::default())
                .probe(Path::new("/src/app"), &SshCancellationToken::default());

            assert_eq!(result, Err(expected));
        }
    }

    #[test]
    fn count_should_stream_chunks_into_the_parser_without_an_output_limit() {
        let output = headers() + "1 .M N... 100644 100644 100644 a b src/main.rs\0? new.rs\0";
        let chunks = output
            .as_bytes()
            .chunks(5)
            .map(<[u8]>::to_vec)
            .collect::<Vec<_>>();
        let runner = FakeRunner::new([Ok((Some(0), chunks))]);

        let summary = reader(&runner, &Arc::default())
            .count(
                Path::new("/src/app"),
                FsmonitorPolicy::Disabled,
                &SshCancellationToken::default(),
            )
            .unwrap();

        assert_eq!(summary.headers.branch.as_deref(), Some("main"));
        assert_eq!(summary.changes.modified, 1);
        assert_eq!(summary.changes.untracked, 1);
        assert_git_request(
            &runner.requests()[0],
            "/src/app",
            true,
            &COUNT_ARGUMENTS,
            None,
            false,
        );
    }

    #[test]
    fn count_with_builtin_fsmonitor_should_keep_its_daemon_and_pin_the_builtin_monitor() {
        let runner = FakeRunner::new([exit(0, &headers())]);

        reader(&runner, &Arc::default())
            .count(
                Path::new("/src/app"),
                FsmonitorPolicy::Builtin,
                &SshCancellationToken::default(),
            )
            .unwrap();

        assert_git_request(
            &runner.requests()[0],
            "/src/app",
            false,
            &COUNT_ARGUMENTS,
            None,
            true,
        );
    }

    #[test]
    fn count_should_report_failed_or_malformed_status() {
        for (response, expected) in [
            (exit(128, ""), RepositoryReadError::Unavailable),
            (exit(0, "nonsense\0"), RepositoryReadError::InvalidResponse),
            (Err(ProgramError::TimedOut), RepositoryReadError::TimedOut),
        ] {
            let runner = FakeRunner::new([response]);

            let result = reader(&runner, &Arc::default()).count(
                Path::new("/src/app"),
                FsmonitorPolicy::Disabled,
                &SshCancellationToken::default(),
            );

            assert_eq!(result, Err(expected));
        }
    }

    #[test]
    fn search_path_should_start_with_the_program_directory() {
        assert_eq!(
            tool_search_path(Path::new("/opt/tools/git")),
            OsString::from("/opt/tools:/usr/bin:/bin")
        );
        assert_eq!(
            tool_search_path(Path::new("git")),
            OsString::from("/usr/bin:/bin")
        );
    }

    #[test]
    fn reader_debug_should_redact_paths() {
        let runner = FakeRunner::new([]);
        let reader = reader(&runner, &Arc::default());

        assert!(!format!("{reader:?}").contains("/"));
    }
}
