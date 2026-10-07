//! The Pull Request query: which repository and branch to ask about, the GitHub CLI arguments and
//! environment, and the filtering of its JSON answer.

use std::ffi::OsString;
use std::fmt;
use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;

use super::display_text::{display_text, sanitize_display_text};
use super::remote_url::parse_remote_url;
use super::{
    GitHubRepository, ProgramError, ProgramExit, PullRequest, PullRequestError, RepositoryConfig,
    RepositoryHead, tool_search_path,
};

const JSON_FIELDS: &str =
    "number,title,isDraft,url,headRefName,baseRefName,headRepositoryOwner,isCrossRepository";
/// The exit status the GitHub CLI uses when no account is logged in.
const NOT_LOGGED_IN_STATUS: i32 = 4;
const MAXIMUM_TITLE_CHARS: usize = 256;
const BASE_REMOTE: &str = "upstream";
const DEFAULT_REMOTE: &str = "origin";

/// One Pull Request lookup: the base repository, the head branch, and the account that owns the
/// head when it lives in a fork.
#[derive(Clone, Eq, Hash, PartialEq)]
pub(crate) struct PullRequestQuery {
    pub(crate) repository: GitHubRepository,
    pub(crate) head_branch: Arc<str>,
    /// The push remote's owner. When known, every pull request must come from it.
    pub(crate) head_owner: Option<Arc<str>>,
}

impl fmt::Debug for PullRequestQuery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PullRequestQuery(<redacted>)")
    }
}

/// The query for the current branch, or `None` when `HEAD` names no published branch or the base
/// remote is not a GitHub-shaped URL.
///
/// The base remote is `upstream` when configured, else the branch's remote, else `origin`. The head
/// branch is where a plain `git push` publishes, as the GitHub CLI reads it: the merge target
/// without `refs/heads/` when `push.default` is `upstream` or `tracking`, else the local branch.
/// The head owner comes from the push remote: the branch's push remote, else
/// `remote.pushdefault`, else the branch's remote.
pub(crate) fn pull_request_query(
    config: &RepositoryConfig,
    head: &RepositoryHead,
) -> Option<PullRequestQuery> {
    let RepositoryHead::Branch(branch) = head else {
        return None;
    };
    let base_remote = if remote_url(config, BASE_REMOTE).is_some() {
        BASE_REMOTE
    } else {
        config.branch_remote.as_deref().unwrap_or(DEFAULT_REMOTE)
    };
    let repository = parse_remote_url(remote_url(config, base_remote)?)?;
    let head_branch = match config.branch_merge.as_deref() {
        Some(merge) if config.push_to_upstream && !merge.is_empty() => {
            let name = merge.strip_prefix("refs/heads/").unwrap_or(merge);
            (!name.is_empty()).then(|| Arc::from(name))?
        }
        _ => Arc::clone(branch),
    };
    let head_owner = config
        .branch_push_remote
        .as_deref()
        .or(config.push_default.as_deref())
        .or(config.branch_remote.as_deref())
        .and_then(|remote| remote_url(config, remote))
        .and_then(parse_remote_url)
        .map(|repository| repository.owner);
    Some(PullRequestQuery {
        repository,
        head_branch,
        head_owner,
    })
}

fn remote_url<'a>(config: &'a RepositoryConfig, name: &str) -> Option<&'a str> {
    config
        .remotes
        .iter()
        .find(|(remote, _)| &**remote == name)
        .map(|(_, url)| &**url)
}

/// `gh pr list` arguments, after the executable.
pub(crate) fn pull_request_arguments(query: &PullRequestQuery) -> Vec<OsString> {
    let GitHubRepository { host, owner, name } = &query.repository;
    [
        "pr".to_owned(),
        "list".to_owned(),
        format!("--repo={host}/{owner}/{name}"),
        format!("--head={}", query.head_branch),
        "--state=open".to_owned(),
        "--limit=10".to_owned(),
        format!("--json={JSON_FIELDS}"),
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}

/// `gh auth status` arguments for one host, after the executable.
pub(crate) fn auth_status_arguments(host: &str) -> Vec<OsString> {
    ["auth", "status", "--hostname", host]
        .into_iter()
        .map(OsString::from)
        .collect()
}

/// The complete GitHub CLI environment: the fixed non-interactive settings, then the composition's
/// passthrough entries, which cannot replace a fixed entry.
pub(crate) fn github_cli_environment(
    executable: &Path,
    home: &Path,
    passthrough: &[(OsString, OsString)],
) -> Vec<(OsString, OsString)> {
    let mut environment: Vec<(OsString, OsString)> = vec![
        ("HOME".into(), home.as_os_str().to_owned()),
        ("PATH".into(), tool_search_path(executable)),
    ];
    environment.extend(
        [
            ("GH_PROMPT_DISABLED", "1"),
            ("GH_NO_UPDATE_NOTIFIER", "1"),
            ("GH_NO_EXTENSION_UPDATE_NOTIFIER", "1"),
            ("NO_COLOR", "1"),
            ("GH_SPINNER_DISABLED", "1"),
            ("GH_PAGER", "cat"),
        ]
        .map(|(name, value)| (name.into(), value.into())),
    );
    for (name, value) in passthrough {
        if !environment.iter().any(|(fixed, _)| fixed == name) {
            environment.push((name.clone(), value.clone()));
        }
    }
    environment
}

/// Exit status 0 carries an answer; 4 means no login; anything else is a failed lookup.
pub(crate) fn pull_request_exit(exit: ProgramExit) -> Result<(), PullRequestError> {
    match exit.code {
        Some(0) => Ok(()),
        Some(NOT_LOGGED_IN_STATUS) => Err(PullRequestError::NotLoggedIn),
        _ => Err(PullRequestError::Unavailable),
    }
}

pub(crate) fn pull_request_program_error(error: ProgramError) -> PullRequestError {
    match error {
        ProgramError::NotFound => PullRequestError::ToolMissing,
        ProgramError::Cancelled => PullRequestError::Cancelled,
        ProgramError::OutputTooLarge => PullRequestError::InvalidResponse,
        ProgramError::TimedOut | ProgramError::Failed => PullRequestError::Unavailable,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListedPullRequest {
    number: u32,
    title: String,
    is_draft: bool,
    url: String,
    head_ref_name: String,
    base_ref_name: String,
    head_repository_owner: Option<ListedOwner>,
    is_cross_repository: bool,
}

#[derive(Deserialize)]
struct ListedOwner {
    login: String,
}

/// The first listed pull request whose head is the queried branch, from the queried head owner
/// when one is known and from the base repository otherwise, with an `https` URL on the queried
/// host.
pub(crate) fn parse_pull_requests(
    output: &[u8],
    query: &PullRequestQuery,
) -> Result<Option<PullRequest>, PullRequestError> {
    let listed: Vec<ListedPullRequest> =
        serde_json::from_slice(output).map_err(|_| PullRequestError::InvalidResponse)?;
    Ok(listed
        .into_iter()
        .find(|listed| {
            listed.head_ref_name == *query.head_branch
                && from_head_owner(listed, query)
                && is_pull_request_url(&listed.url, &query.repository.host)
        })
        .map(|listed| PullRequest {
            number: listed.number,
            title: sanitize_display_text(&listed.title)
                .chars()
                .take(MAXIMUM_TITLE_CHARS)
                .collect::<String>()
                .into(),
            draft: listed.is_draft,
            url: listed.url.into(),
            head: display_text(listed.head_ref_name.as_bytes()),
            base: display_text(listed.base_ref_name.as_bytes()),
        }))
}

fn from_head_owner(listed: &ListedPullRequest, query: &PullRequestQuery) -> bool {
    let Some(expected) = &query.head_owner else {
        return !listed.is_cross_repository;
    };
    match &listed.head_repository_owner {
        Some(owner) => owner.login.eq_ignore_ascii_case(expected),
        // A same-repository pull request's head owner is the base repository's owner.
        None => {
            !listed.is_cross_repository && query.repository.owner.eq_ignore_ascii_case(expected)
        }
    }
}

fn is_pull_request_url(url: &str, host: &str) -> bool {
    let Some((authority, path)) = url
        .strip_prefix("https://")
        .and_then(|rest| rest.split_once('/'))
    else {
        return false;
    };
    authority.eq_ignore_ascii_case(host)
        && !path.is_empty()
        && !url
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;

    fn config(remotes: &[(&str, &str)]) -> RepositoryConfig {
        RepositoryConfig {
            remotes: remotes
                .iter()
                .map(|(name, url)| (Arc::from(*name), Arc::from(*url)))
                .collect(),
            ..RepositoryConfig::default()
        }
    }

    fn branch(name: &str) -> RepositoryHead {
        RepositoryHead::Branch(name.into())
    }

    fn github(host: &str, owner: &str, name: &str) -> GitHubRepository {
        GitHubRepository {
            host: host.into(),
            owner: owner.into(),
            name: name.into(),
        }
    }

    fn query(head_owner: Option<&str>) -> PullRequestQuery {
        PullRequestQuery {
            repository: github("github.com", "org", "app"),
            head_branch: "feature".into(),
            head_owner: head_owner.map(Into::into),
        }
    }

    #[test]
    fn query_should_use_origin_and_the_local_branch_by_default() {
        let config = config(&[("origin", "git@github.com:me/app.git")]);

        assert_eq!(
            pull_request_query(&config, &branch("feature")),
            Some(PullRequestQuery {
                repository: github("github.com", "me", "app"),
                head_branch: "feature".into(),
                head_owner: None,
            })
        );
    }

    #[test]
    fn query_should_prefer_the_upstream_remote_and_take_the_owner_from_the_branch_remote() {
        let config = RepositoryConfig {
            branch_remote: Some("origin".into()),
            branch_merge: Some("refs/heads/topic".into()),
            push_to_upstream: true,
            ..config(&[
                ("origin", "git@github.com:me/app.git"),
                ("upstream", "https://github.com/org/app.git"),
            ])
        };

        assert_eq!(
            pull_request_query(&config, &branch("local-topic")),
            Some(PullRequestQuery {
                repository: github("github.com", "org", "app"),
                head_branch: "topic".into(),
                head_owner: Some("me".into()),
            })
        );
    }

    #[test]
    fn query_should_name_the_pushed_branch_when_it_tracks_another_name() {
        let config = RepositoryConfig {
            branch_remote: Some("origin".into()),
            branch_merge: Some("refs/heads/main".into()),
            ..config(&[("origin", "git@github.com:me/app.git")])
        };

        let query = pull_request_query(&config, &branch("feature")).unwrap();

        assert_eq!(&*query.head_branch, "feature");
    }

    #[test]
    fn query_should_use_the_branch_remote_when_there_is_no_upstream_remote() {
        let config = RepositoryConfig {
            branch_remote: Some("work".into()),
            ..config(&[
                ("origin", "git@github.com:me/app.git"),
                ("work", "https://acme.ghe.com/team/app"),
            ])
        };

        let query = pull_request_query(&config, &branch("main")).unwrap();

        assert_eq!(query.repository, github("acme.ghe.com", "team", "app"));
        assert_eq!(query.head_owner.as_deref(), Some("team"));
    }

    #[test]
    fn head_owner_should_follow_push_remote_precedence() {
        let remotes = [
            ("origin", "git@github.com:origin-owner/app.git"),
            ("default", "git@github.com:default-owner/app.git"),
            ("push", "git@github.com:push-owner/app.git"),
        ];
        let owner = |push_remote: Option<&str>, push_default: Option<&str>| {
            let config = RepositoryConfig {
                branch_remote: Some("origin".into()),
                branch_push_remote: push_remote.map(Into::into),
                push_default: push_default.map(Into::into),
                ..config(&remotes)
            };
            pull_request_query(&config, &branch("feature"))
                .unwrap()
                .head_owner
        };

        assert_eq!(
            owner(Some("push"), Some("default")).as_deref(),
            Some("push-owner")
        );
        assert_eq!(
            owner(None, Some("default")).as_deref(),
            Some("default-owner")
        );
        assert_eq!(owner(None, None).as_deref(), Some("origin-owner"));
        assert_eq!(owner(Some("missing"), None), None);
    }

    #[test]
    fn head_branch_should_keep_a_merge_value_without_the_heads_prefix() {
        let config = RepositoryConfig {
            branch_merge: Some("release/2".into()),
            push_to_upstream: true,
            ..config(&[("origin", "https://github.com/me/app")])
        };

        assert_eq!(
            pull_request_query(&config, &branch("local"))
                .unwrap()
                .head_branch
                .as_ref(),
            "release/2"
        );
    }

    #[test]
    fn no_query_should_be_made_without_a_published_branch_or_github_shaped_remote() {
        let origin = config(&[("origin", "https://github.com/me/app")]);
        assert_eq!(
            pull_request_query(&origin, &RepositoryHead::Detached("1bccb23".into())),
            None
        );
        assert_eq!(
            pull_request_query(&origin, &RepositoryHead::Unborn("main".into())),
            None
        );
        assert_eq!(pull_request_query(&config(&[]), &branch("main")), None);
        assert_eq!(
            pull_request_query(&config(&[("origin", "/srv/git/app.git")]), &branch("main")),
            None
        );
        let local_tracking = RepositoryConfig {
            branch_remote: Some(".".into()),
            ..config(&[("other", "https://github.com/me/app")])
        };
        assert_eq!(pull_request_query(&local_tracking, &branch("main")), None);
        let empty_merge = RepositoryConfig {
            branch_merge: Some("refs/heads/".into()),
            push_to_upstream: true,
            ..origin
        };
        assert_eq!(pull_request_query(&empty_merge, &branch("main")), None);
    }

    #[test]
    fn arguments_should_name_the_host_repository_and_branch() {
        let query = PullRequestQuery {
            head_branch: "-dash/branch".into(),
            ..query(None)
        };

        assert_eq!(
            pull_request_arguments(&query),
            [
                "pr",
                "list",
                "--repo=github.com/org/app",
                "--head=-dash/branch",
                "--state=open",
                "--limit=10",
                "--json=number,title,isDraft,url,headRefName,baseRefName,headRepositoryOwner,isCrossRepository",
            ]
        );
        assert_eq!(
            auth_status_arguments("ghe.example.com"),
            ["auth", "status", "--hostname", "ghe.example.com"]
        );
    }

    #[test]
    fn environment_should_be_fixed_then_passthrough_without_overrides() {
        let passthrough = [
            (
                OsString::from("HTTPS_PROXY"),
                OsString::from("http://proxy:8080"),
            ),
            (OsString::from("PATH"), OsString::from("/evil")),
            (OsString::from("GH_PAGER"), OsString::from("less")),
            (
                OsString::from("GH_CONFIG_DIR"),
                OsString::from("/config/gh"),
            ),
        ];

        let environment = github_cli_environment(
            &PathBuf::from("/tools/bin/gh"),
            &PathBuf::from("/home/person"),
            &passthrough,
        );

        let expected: Vec<(OsString, OsString)> = [
            ("HOME", "/home/person"),
            ("PATH", "/tools/bin:/usr/bin:/bin"),
            ("GH_PROMPT_DISABLED", "1"),
            ("GH_NO_UPDATE_NOTIFIER", "1"),
            ("GH_NO_EXTENSION_UPDATE_NOTIFIER", "1"),
            ("NO_COLOR", "1"),
            ("GH_SPINNER_DISABLED", "1"),
            ("GH_PAGER", "cat"),
            ("HTTPS_PROXY", "http://proxy:8080"),
            ("GH_CONFIG_DIR", "/config/gh"),
        ]
        .map(|(name, value)| (name.into(), value.into()))
        .into();
        assert_eq!(environment, expected);
    }

    #[test]
    fn exits_and_program_errors_should_map_to_lookup_failures() {
        assert_eq!(pull_request_exit(ProgramExit { code: Some(0) }), Ok(()));
        assert_eq!(
            pull_request_exit(ProgramExit { code: Some(4) }),
            Err(PullRequestError::NotLoggedIn)
        );
        for code in [Some(1), Some(2), Some(128), None] {
            assert_eq!(
                pull_request_exit(ProgramExit { code }),
                Err(PullRequestError::Unavailable)
            );
        }
        for (error, expected) in [
            (ProgramError::NotFound, PullRequestError::ToolMissing),
            (ProgramError::Cancelled, PullRequestError::Cancelled),
            (ProgramError::TimedOut, PullRequestError::Unavailable),
            (
                ProgramError::OutputTooLarge,
                PullRequestError::InvalidResponse,
            ),
            (ProgramError::Failed, PullRequestError::Unavailable),
        ] {
            assert_eq!(pull_request_program_error(error), expected);
        }
    }

    fn listed(
        number: u32,
        head: &str,
        cross: bool,
        owner: Option<&str>,
        url: &str,
    ) -> serde_json::Value {
        json!({
            "number": number,
            "title": format!("Title {number}"),
            "isDraft": number.is_multiple_of(2),
            "url": url,
            "headRefName": head,
            "baseRefName": "main",
            "headRepositoryOwner": owner.map(|login| json!({ "id": "x", "login": login })),
            "isCrossRepository": cross,
        })
    }

    fn parse(entries: &[serde_json::Value], query: &PullRequestQuery) -> Option<PullRequest> {
        let output = serde_json::to_vec(entries).unwrap();
        parse_pull_requests(&output, query).unwrap()
    }

    #[test]
    fn the_first_matching_pull_request_should_be_returned() {
        let entries = [
            listed(
                7,
                "other",
                false,
                Some("org"),
                "https://github.com/org/app/pull/7",
            ),
            listed(
                8,
                "feature",
                false,
                Some("org"),
                "https://github.com/org/app/pull/8",
            ),
            listed(
                9,
                "feature",
                false,
                Some("org"),
                "https://github.com/org/app/pull/9",
            ),
        ];

        assert_eq!(
            parse(&entries, &query(None)),
            Some(PullRequest {
                number: 8,
                title: "Title 8".into(),
                draft: true,
                url: "https://github.com/org/app/pull/8".into(),
                head: "feature".into(),
                base: "main".into(),
            })
        );
    }

    #[test]
    fn a_fork_branch_should_not_match_the_base_repository_branch_of_the_same_name() {
        let entries = [
            listed(
                1,
                "feature",
                false,
                Some("org"),
                "https://github.com/org/app/pull/1",
            ),
            listed(
                2,
                "feature",
                true,
                Some("me"),
                "https://github.com/org/app/pull/2",
            ),
        ];

        assert_eq!(
            parse(&entries, &query(Some("me"))).map(|pr| pr.number),
            Some(2)
        );
        assert_eq!(
            parse(&entries, &query(Some("org"))).map(|pr| pr.number),
            Some(1)
        );
        assert_eq!(parse(&entries, &query(None)).map(|pr| pr.number), Some(1));
    }

    #[test]
    fn cross_repository_pull_requests_should_come_from_the_head_owner() {
        let entries = [
            listed(
                1,
                "feature",
                true,
                Some("stranger"),
                "https://github.com/org/app/pull/1",
            ),
            listed(
                2,
                "feature",
                true,
                None,
                "https://github.com/org/app/pull/2",
            ),
            listed(
                3,
                "feature",
                true,
                Some("Me"),
                "https://github.com/org/app/pull/3",
            ),
        ];

        assert_eq!(
            parse(&entries, &query(Some("me"))).map(|pr| pr.number),
            Some(3)
        );
        assert_eq!(parse(&entries, &query(None)), None);
    }

    #[test]
    fn pull_request_urls_should_be_https_on_the_queried_host() {
        for url in [
            "http://github.com/org/app/pull/1",
            "https://evil.example/org/app/pull/1",
            "https://github.com.evil.example/org/app/pull/1",
            "https://user@github.com/org/app/pull/1",
            "https://github.com:8443/org/app/pull/1",
            "https://github.com/",
            "https://github.com",
            "javascript:alert(1)",
            "https://github.com/org/app/pull/1\n",
            "",
        ] {
            let entries = [listed(1, "feature", false, None, url)];
            assert_eq!(parse(&entries, &query(None)), None, "{url:?}");
        }
        let entries = [listed(
            1,
            "feature",
            false,
            None,
            "https://GitHub.com/org/app/pull/1",
        )];
        assert!(parse(&entries, &query(None)).is_some());
    }

    #[test]
    fn titles_and_branch_names_should_be_sanitized_and_titles_bounded() {
        let mut entry = listed(
            1,
            "feature",
            false,
            None,
            "https://github.com/org/app/pull/1",
        );
        entry["title"] = json!(format!("Fix\u{202e}\u{1b}[2J {}", "x".repeat(400)));
        entry["baseRefName"] = json!("ma\u{2066}in");

        let pull_request = parse(&[entry], &query(None)).unwrap();

        assert!(pull_request.title.starts_with("Fix\u{fffd}\u{fffd}[2J x"));
        assert_eq!(pull_request.title.chars().count(), MAXIMUM_TITLE_CHARS);
        assert_eq!(&*pull_request.base, "ma\u{fffd}in");
    }

    #[test]
    fn an_empty_list_should_mean_no_pull_request() {
        assert_eq!(parse_pull_requests(b"[]\n", &query(None)), Ok(None));
    }

    #[test]
    fn malformed_json_should_be_an_invalid_response() {
        for output in [
            &b""[..],
            b"{}",
            b"[{\"number\": 1}]",
            b"[{\"number\": -1, \"title\": \"\", \"isDraft\": false, \"url\": \"\", \"headRefName\": \"\", \"baseRefName\": \"\", \"headRepositoryOwner\": null, \"isCrossRepository\": false}]",
            b"not json",
            b"[",
        ] {
            assert_eq!(
                parse_pull_requests(output, &query(None)),
                Err(PullRequestError::InvalidResponse),
                "{output:?}"
            );
        }
    }

    #[test]
    fn query_debug_should_redact_repository_and_branch() {
        let query = PullRequestQuery {
            head_branch: "secret-branch".into(),
            ..query(Some("secret-owner"))
        };

        assert!(!format!("{query:?}").contains("secret"));
    }
}
