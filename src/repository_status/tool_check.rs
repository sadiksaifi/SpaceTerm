//! The readiness check behind the Git Settings Section.

use std::ffi::OsString;
use std::path::Path;
use std::time::Instant;

use super::local_read::git_environment;
use super::pull_request::github_cli_environment;
use super::tools::{git_tool_status, parse_git_version};
use super::{
    GitHubCliStatus, GitToolStatus, ProgramError, ProgramRequest, RepositoryProgramRunner,
    ToolInventory,
};
use crate::ssh::cancellation::SshCancellationToken;

const TOOL_CHECK_OUTPUT_LIMIT: usize = 64 * 1024;
/// Every host's login state as JSON. The CLI exits 0 unless it fails.
const AUTH_STATUS_JSON_ARGUMENTS: [&str; 4] = ["auth", "status", "--json", "hosts"];
/// The same question for a GitHub CLI without `--json`. It exits 0 only when every known host is
/// signed in.
const AUTH_STATUS_ARGUMENTS: [&str; 2] = ["auth", "status"];

/// Runs `git --version` and `gh auth status` once each. Blocking.
///
/// Git that cannot be run or reports no version counts as not found. The GitHub CLI is signed in
/// when any host, such as github.com or a GitHub Enterprise Server, has an active working login.
pub(crate) fn check_tools(
    runner: &dyn RepositoryProgramRunner,
    inventory: &ToolInventory,
    home: &Path,
    github_cli_passthrough: &[(OsString, OsString)],
    deadline: Instant,
    cancellation: &SshCancellationToken,
) -> (GitToolStatus, GitHubCliStatus) {
    let git = inventory
        .git
        .as_deref()
        .map_or(GitToolStatus::NotFound, |git| {
            let request = ProgramRequest {
                executable: git.to_path_buf(),
                arguments: vec!["--version".into()],
                directory: home.to_path_buf(),
                environment: git_environment(git, home),
                stdout_limit: Some(TOOL_CHECK_OUTPUT_LIMIT),
                deadline: Some(deadline),
                keep_process_group_on_exit: false,
            };
            let mut output = Vec::new();
            match runner.run(
                &request,
                &mut |chunk| output.extend_from_slice(chunk),
                cancellation,
            ) {
                Ok(exit) if exit.success() => {
                    parse_git_version(&output).map_or(GitToolStatus::NotFound, git_tool_status)
                }
                _ => GitToolStatus::NotFound,
            }
        });
    let github_cli = inventory
        .github_cli
        .as_deref()
        .map_or(GitHubCliStatus::NotFound, |github_cli| {
            let run = |arguments: &[&str], output: &mut Vec<u8>| {
                let request = ProgramRequest {
                    executable: github_cli.to_path_buf(),
                    arguments: arguments.iter().map(OsString::from).collect(),
                    directory: home.to_path_buf(),
                    environment: github_cli_environment(github_cli, home, github_cli_passthrough),
                    stdout_limit: Some(TOOL_CHECK_OUTPUT_LIMIT),
                    deadline: Some(deadline),
                    keep_process_group_on_exit: false,
                };
                runner.run(
                    &request,
                    &mut |chunk| output.extend_from_slice(chunk),
                    cancellation,
                )
            };
            let mut output = Vec::new();
            match run(&AUTH_STATUS_JSON_ARGUMENTS, &mut output) {
                Ok(exit) if exit.success() => {
                    return match any_host_signed_in(&output) {
                        Some(true) => GitHubCliStatus::SignedIn,
                        Some(false) => GitHubCliStatus::SignedOut,
                        None => GitHubCliStatus::Unavailable,
                    };
                }
                Ok(_) => {}
                Err(error) => return github_cli_error(error),
            }
            // A GitHub CLI without `--json` exits with an error; ask it the older way.
            match run(&AUTH_STATUS_ARGUMENTS, &mut Vec::new()) {
                Ok(exit) if exit.success() => GitHubCliStatus::SignedIn,
                Ok(_) => GitHubCliStatus::SignedOut,
                Err(error) => github_cli_error(error),
            }
        });
    (git, github_cli)
}

fn github_cli_error(error: ProgramError) -> GitHubCliStatus {
    match error {
        ProgramError::NotFound => GitHubCliStatus::NotFound,
        _ => GitHubCliStatus::Unavailable,
    }
}

/// Reads `{"hosts": {"<host>": [{"state": "success", "active": true}, ...]}}`. Nothing else in
/// the output is read or kept.
fn any_host_signed_in(output: &[u8]) -> Option<bool> {
    #[derive(serde::Deserialize)]
    struct Status {
        hosts: std::collections::BTreeMap<String, Vec<Account>>,
    }
    #[derive(serde::Deserialize)]
    struct Account {
        #[serde(default)]
        state: String,
        #[serde(default)]
        active: bool,
    }
    let status: Status = serde_json::from_slice(output).ok()?;
    Some(
        status
            .hosts
            .values()
            .flatten()
            .any(|account| account.active && account.state == "success"),
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use super::*;
    use crate::repository_status::testing::{FakeResponse, FakeRunner, exit};
    use crate::repository_status::{MINIMUM_GIT_VERSION, ToolVersion};

    const GIT: &str = "/tools/git/bin/git";
    const GH: &str = "/tools/gh/bin/gh";
    const HOME: &str = "/home/person";
    const VERSION: &str = "git version 2.47.0\n";
    const SIGNED_IN: &str = r#"{"hosts":{"github.com":[{"state":"success","active":true,"host":"github.com","login":"person"}]}}"#;

    fn inventory(git: bool, github_cli: bool) -> ToolInventory {
        ToolInventory {
            git: git.then(|| PathBuf::from(GIT)),
            github_cli: github_cli.then(|| PathBuf::from(GH)),
        }
    }

    fn passthrough() -> Vec<(OsString, OsString)> {
        vec![("HTTPS_PROXY".into(), "http://proxy:3128".into())]
    }

    fn check(
        inventory: &ToolInventory,
        responses: impl IntoIterator<Item = FakeResponse>,
    ) -> ((GitToolStatus, GitHubCliStatus), Vec<ProgramRequest>) {
        let runner = FakeRunner::new(responses);
        let statuses = check_tools(
            &*runner,
            inventory,
            Path::new(HOME),
            &passthrough(),
            Instant::now(),
            &SshCancellationToken::default(),
        );
        (statuses, runner.requests())
    }

    #[test]
    fn check_should_run_each_tool_with_its_environment_and_bounds() {
        let runner = FakeRunner::new([exit(0, VERSION), exit(0, SIGNED_IN)]);
        let deadline = Instant::now() + Duration::from_secs(5);

        let statuses = check_tools(
            &*runner,
            &inventory(true, true),
            Path::new(HOME),
            &passthrough(),
            deadline,
            &SshCancellationToken::default(),
        );

        let version = ToolVersion {
            major: 2,
            minor: 47,
            patch: 0,
        };
        assert_eq!(
            statuses,
            (GitToolStatus::Ready(version), GitHubCliStatus::SignedIn)
        );
        let requests = runner.requests();
        let [git, github_cli] = requests.as_slice() else {
            panic!("expected two runs");
        };
        assert_eq!(git.executable, Path::new(GIT));
        assert_eq!(git.arguments, [OsString::from("--version")]);
        assert_eq!(git.directory, Path::new(HOME));
        assert_eq!(
            git.environment,
            git_environment(Path::new(GIT), Path::new(HOME))
        );
        assert_eq!(github_cli.executable, Path::new(GH));
        assert_eq!(
            github_cli.arguments,
            ["auth", "status", "--json", "hosts"].map(OsString::from)
        );
        assert_eq!(github_cli.directory, Path::new(HOME));
        assert_eq!(
            github_cli.environment,
            github_cli_environment(Path::new(GH), Path::new(HOME), &passthrough())
        );
        for request in [git, github_cli] {
            assert_eq!(request.stdout_limit, Some(64 * 1024));
            assert_eq!(request.deadline, Some(deadline));
            assert!(!request.keep_process_group_on_exit);
        }
    }

    #[test]
    fn missing_tools_should_be_not_found_without_running_anything() {
        let (statuses, requests) = check(&inventory(false, false), []);

        assert_eq!(
            statuses,
            (GitToolStatus::NotFound, GitHubCliStatus::NotFound)
        );
        assert!(requests.is_empty());
    }

    #[test]
    fn git_results_should_map_to_tool_status() {
        let old = ToolVersion {
            major: 2,
            minor: 14,
            patch: 0,
        };
        for (response, expected) in [
            (exit(0, "git version 2.14.0\n"), GitToolStatus::TooOld(old)),
            (
                exit(0, "git version 2.15.0\n"),
                GitToolStatus::Ready(MINIMUM_GIT_VERSION),
            ),
            (exit(0, "no developer tools\n"), GitToolStatus::NotFound),
            (exit(1, VERSION), GitToolStatus::NotFound),
            (Err(ProgramError::NotFound), GitToolStatus::NotFound),
            (Err(ProgramError::Failed), GitToolStatus::NotFound),
            (Err(ProgramError::TimedOut), GitToolStatus::NotFound),
        ] {
            let ((git, github_cli), _) = check(&inventory(true, false), [response]);

            assert_eq!(git, expected);
            assert_eq!(github_cli, GitHubCliStatus::NotFound);
        }
    }

    #[test]
    fn github_cli_results_should_map_to_login_status() {
        let enterprise_only = r#"{"hosts":{
            "github.com":[{"state":"error","active":true}],
            "ghe.example.com":[{"state":"success","active":true}]}}"#;
        let inactive_only = r#"{"hosts":{"github.com":[
            {"state":"error","active":true},{"state":"success","active":false}]}}"#;
        for (response, expected) in [
            (exit(0, SIGNED_IN), GitHubCliStatus::SignedIn),
            (exit(0, enterprise_only), GitHubCliStatus::SignedIn),
            (exit(0, inactive_only), GitHubCliStatus::SignedOut),
            (exit(0, r#"{"hosts":{}}"#), GitHubCliStatus::SignedOut),
            (exit(0, "not json"), GitHubCliStatus::Unavailable),
            (Err(ProgramError::NotFound), GitHubCliStatus::NotFound),
            (Err(ProgramError::TimedOut), GitHubCliStatus::Unavailable),
            (Err(ProgramError::Failed), GitHubCliStatus::Unavailable),
            (Err(ProgramError::Cancelled), GitHubCliStatus::Unavailable),
            (
                Err(ProgramError::OutputTooLarge),
                GitHubCliStatus::Unavailable,
            ),
        ] {
            let ((git, github_cli), requests) = check(&inventory(false, true), [response]);

            assert_eq!(git, GitToolStatus::NotFound);
            assert_eq!(github_cli, expected);
            assert_eq!(requests.len(), 1);
        }
    }

    #[test]
    fn a_github_cli_without_json_should_be_asked_without_it() {
        for (response, expected) in [
            (exit(0, ""), GitHubCliStatus::SignedIn),
            (exit(1, ""), GitHubCliStatus::SignedOut),
            (Ok((None, Vec::new())), GitHubCliStatus::SignedOut),
            (Err(ProgramError::TimedOut), GitHubCliStatus::Unavailable),
        ] {
            let ((_, github_cli), requests) = check(
                &inventory(false, true),
                [exit(1, "unknown flag: --json\n"), response],
            );

            assert_eq!(github_cli, expected);
            assert_eq!(
                requests[1].arguments,
                ["auth", "status"].map(OsString::from)
            );
        }
    }
}
