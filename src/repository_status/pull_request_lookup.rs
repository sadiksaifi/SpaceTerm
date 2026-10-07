//! The GitHub CLI call policy for Pull Request lookups.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::pull_request::{
    PullRequestQuery, auth_status_arguments, github_cli_environment, parse_pull_requests,
    pull_request_arguments, pull_request_exit, pull_request_program_error,
};
use super::remote_url::{GitHubHost, github_host};
use super::{ProgramExit, ProgramRequest, PullRequest, PullRequestError, RepositoryProgramRunner};
use crate::ssh::cancellation::SshCancellationToken;

pub(crate) const PULL_REQUEST_OUTPUT_LIMIT: usize = 256 * 1024;
pub(crate) const PULL_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Runs the local GitHub CLI with its existing login. Blocking; callers use a dedicated thread.
pub(crate) struct GitHubCliLookup {
    runner: Arc<dyn RepositoryProgramRunner>,
    executable: PathBuf,
    home: PathBuf,
    /// Environment entries the GitHub CLI may need, such as proxy settings, captured at
    /// composition.
    passthrough: Vec<(OsString, OsString)>,
}

impl fmt::Debug for GitHubCliLookup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GitHubCliLookup(<redacted>)")
    }
}

impl GitHubCliLookup {
    pub(crate) fn new(
        runner: Arc<dyn RepositoryProgramRunner>,
        executable: PathBuf,
        home: PathBuf,
        passthrough: Vec<(OsString, OsString)>,
    ) -> Self {
        Self {
            runner,
            executable,
            home,
            passthrough,
        }
    }

    /// The open or draft pull request for the query's branch. The run ends
    /// [`PULL_REQUEST_TIMEOUT`] after `now`.
    pub(crate) fn find_pull_request(
        &self,
        query: &PullRequestQuery,
        now: Instant,
        cancellation: &SshCancellationToken,
    ) -> Result<Option<PullRequest>, PullRequestError> {
        let mut output = Vec::new();
        let exit = self.run(
            pull_request_arguments(query),
            now,
            &mut output,
            cancellation,
        )?;
        pull_request_exit(exit)?;
        parse_pull_requests(&output, query)
    }

    /// Whether the GitHub CLI is logged in to `host`, which makes it a GitHub host.
    pub(crate) fn is_github_host(
        &self,
        host: &str,
        now: Instant,
        cancellation: &SshCancellationToken,
    ) -> Result<bool, PullRequestError> {
        let exit = self.run(
            auth_status_arguments(host),
            now,
            &mut Vec::new(),
            cancellation,
        )?;
        Ok(exit.success())
    }

    fn run(
        &self,
        arguments: Vec<OsString>,
        now: Instant,
        output: &mut Vec<u8>,
        cancellation: &SshCancellationToken,
    ) -> Result<ProgramExit, PullRequestError> {
        let request = ProgramRequest {
            executable: self.executable.clone(),
            arguments,
            directory: self.home.clone(),
            environment: github_cli_environment(&self.executable, &self.home, &self.passthrough),
            stdout_limit: Some(PULL_REQUEST_OUTPUT_LIMIT),
            deadline: Some(now + PULL_REQUEST_TIMEOUT),
            keep_process_group_on_exit: false,
        };
        self.runner
            .run(
                &request,
                &mut |chunk| output.extend_from_slice(chunk),
                cancellation,
            )
            .map_err(pull_request_program_error)
    }
}

/// The hosts the GitHub CLI confirmed as GitHub hosts during this app session. A host it was not
/// logged in to, or could not reach, is asked again on the next lookup, so a later login or
/// connection finds its Pull Requests.
#[derive(Default)]
pub(crate) struct GitHubHosts(Mutex<HashSet<Arc<str>>>);

impl GitHubHosts {
    /// Whether `host` is a GitHub host. Blocking when the GitHub CLI must be asked.
    pub(crate) fn is_github_host(
        &self,
        lookup: &GitHubCliLookup,
        host: &Arc<str>,
        cancellation: &SshCancellationToken,
    ) -> Result<bool, PullRequestError> {
        if github_host(host) == GitHubHost::Known || self.confirmed().contains(host) {
            return Ok(true);
        }
        let confirmed = lookup.is_github_host(host, Instant::now(), cancellation)?;
        if confirmed {
            self.confirmed().insert(Arc::clone(host));
        }
        Ok(confirmed)
    }

    fn confirmed(&self) -> std::sync::MutexGuard<'_, HashSet<Arc<str>>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::repository_status::testing::{FakeRunner, exit};
    use crate::repository_status::{GitHubRepository, ProgramError};

    fn lookup(runner: &Arc<FakeRunner>) -> GitHubCliLookup {
        GitHubCliLookup::new(
            Arc::clone(runner) as Arc<dyn RepositoryProgramRunner>,
            PathBuf::from("/tools/gh/bin/gh"),
            PathBuf::from("/home/person"),
            vec![("HTTPS_PROXY".into(), "http://proxy:3128".into())],
        )
    }

    fn query() -> PullRequestQuery {
        PullRequestQuery {
            repository: GitHubRepository {
                host: "github.com".into(),
                owner: "org".into(),
                name: "app".into(),
            },
            head_branch: "feature".into(),
            head_owner: None,
        }
    }

    #[test]
    fn a_host_the_github_cli_could_not_confirm_should_be_asked_again() {
        let runner = FakeRunner::new([exit(1, ""), exit(0, "")]);
        let lookup = lookup(&runner);
        let hosts = GitHubHosts::default();
        let host: Arc<str> = "git.example.com".into();
        let cancellation = SshCancellationToken::default();

        // Logged out of the host, or its network is unreachable.
        assert!(!hosts.is_github_host(&lookup, &host, &cancellation).unwrap());
        // The person logs in; the next lookup asks again and remembers the answer.
        assert!(hosts.is_github_host(&lookup, &host, &cancellation).unwrap());
        assert!(hosts.is_github_host(&lookup, &host, &cancellation).unwrap());
        assert!(
            hosts
                .is_github_host(&lookup, &"github.com".into(), &cancellation)
                .unwrap()
        );

        let arguments: Vec<_> = runner
            .requests()
            .into_iter()
            .map(|request| request.arguments)
            .collect();
        assert_eq!(
            arguments,
            vec![
                auth_status_arguments("git.example.com"),
                auth_status_arguments("git.example.com"),
            ]
        );
    }

    const LISTED: &str = r#"[{"number":478,"title":"Add Repository Status","isDraft":true,
        "url":"https://github.com/org/app/pull/478","headRefName":"feature","baseRefName":"main",
        "headRepositoryOwner":{"id":"1","login":"org"},"isCrossRepository":false}]"#;

    #[test]
    fn find_should_run_gh_with_bounds_and_return_the_pull_request() {
        let runner = FakeRunner::new([exit(0, LISTED)]);
        let now = Instant::now();

        let pull_request = lookup(&runner)
            .find_pull_request(&query(), now, &SshCancellationToken::default())
            .unwrap()
            .unwrap();

        assert_eq!(pull_request.number, 478);
        assert!(pull_request.draft);
        let requests = runner.requests();
        let [request] = requests.as_slice() else {
            panic!("expected one run");
        };
        assert_eq!(request.executable, Path::new("/tools/gh/bin/gh"));
        assert_eq!(request.arguments, pull_request_arguments(&query()));
        assert_eq!(request.directory, Path::new("/home/person"));
        assert_eq!(
            request.environment,
            github_cli_environment(
                Path::new("/tools/gh/bin/gh"),
                Path::new("/home/person"),
                &[("HTTPS_PROXY".into(), "http://proxy:3128".into())],
            )
        );
        assert_eq!(request.stdout_limit, Some(256 * 1024));
        assert_eq!(request.deadline, Some(now + Duration::from_secs(20)));
        assert!(!request.keep_process_group_on_exit);
    }

    #[test]
    fn find_should_map_exits_and_runner_failures() {
        for (response, expected) in [
            (exit(4, ""), Err(PullRequestError::NotLoggedIn)),
            (exit(1, LISTED), Err(PullRequestError::Unavailable)),
            (exit(0, "[]"), Ok(None)),
            (exit(0, "{"), Err(PullRequestError::InvalidResponse)),
            (
                Err(ProgramError::NotFound),
                Err(PullRequestError::ToolMissing),
            ),
            (
                Err(ProgramError::TimedOut),
                Err(PullRequestError::Unavailable),
            ),
            (
                Err(ProgramError::Cancelled),
                Err(PullRequestError::Cancelled),
            ),
            (
                Err(ProgramError::OutputTooLarge),
                Err(PullRequestError::InvalidResponse),
            ),
        ] {
            let runner = FakeRunner::new([response]);

            let result = lookup(&runner).find_pull_request(
                &query(),
                Instant::now(),
                &SshCancellationToken::default(),
            );

            assert_eq!(result, expected);
        }
    }

    #[test]
    fn host_check_should_run_auth_status_for_the_host() {
        for (response, expected) in [
            (exit(0, ""), Ok(true)),
            (exit(1, ""), Ok(false)),
            (exit(4, ""), Ok(false)),
            (
                Err(ProgramError::NotFound),
                Err(PullRequestError::ToolMissing),
            ),
            (
                Err(ProgramError::Failed),
                Err(PullRequestError::Unavailable),
            ),
        ] {
            let runner = FakeRunner::new([response]);
            let now = Instant::now();

            let result = lookup(&runner).is_github_host(
                "ghe.example.com",
                now,
                &SshCancellationToken::default(),
            );

            assert_eq!(result, expected);
            let request = &runner.requests()[0];
            assert_eq!(request.arguments, auth_status_arguments("ghe.example.com"));
            assert_eq!(request.deadline, Some(now + PULL_REQUEST_TIMEOUT));
            assert_eq!(request.stdout_limit, Some(PULL_REQUEST_OUTPUT_LIMIT));
        }
    }

    #[test]
    fn lookup_debug_should_redact_paths_and_environment() {
        let runner = FakeRunner::new([]);

        assert_eq!(
            format!("{:?}", lookup(&runner)),
            "GitHubCliLookup(<redacted>)"
        );
    }
}
