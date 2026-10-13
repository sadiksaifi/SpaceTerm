use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use spaceterm_core::identity::{PaneId, ServerId, WorkspaceId, WorktreeId};
use spaceterm_core::organization::BranchName;
use spaceterm_core::worktrees::queries::GitPullRequestFacts;

use super::*;
use crate::repository_status::RepositoryProgramRunner;
use crate::repository_status::pull_request_lookup::{
    PULL_REQUEST_OUTPUT_LIMIT, PULL_REQUEST_TIMEOUT,
};
use crate::repository_status::testing::{FakeRunner, exit};

const LISTED: &str = r#"[{"number":520,"title":"Persistent servers","isDraft":false,
    "url":"https://github.com/base/app/pull/520","headRefName":"published","baseRefName":"main",
    "headRepositoryOwner":{"login":"fork"},"isCrossRepository":true}]"#;

fn repository(server: u8, local: u64) -> RepositoryId {
    RepositoryId::new(ServerId::new([server; 16]), local).unwrap()
}

fn target(repository: RepositoryId) -> GitStatusTarget {
    GitStatusTarget::Worktree(WorktreeId::new(repository.server(), repository.local()).unwrap())
}

fn facts(branch: &str) -> GitStatusFacts {
    GitStatusFacts {
        revision: NonZeroU64::new(1).unwrap(),
        branch: Some(BranchName::new(branch.into()).unwrap()),
        changes: Vec::new(),
        dirty: false,
        unchecked: false,
        pull_request: Some(
            GitPullRequestFacts::new(
                "github.com".into(),
                "base".into(),
                "app".into(),
                BranchName::new("published".into()).unwrap(),
                Some("fork".into()),
            )
            .unwrap(),
        ),
        upstream: None,
        operation: None,
        counts: None,
    }
}

fn lookup(runner: &Arc<FakeRunner>) -> GitHubCliLookup {
    GitHubCliLookup::new(
        Arc::clone(runner) as Arc<dyn RepositoryProgramRunner>,
        PathBuf::from("/client-tools/gh/bin/gh"),
        PathBuf::from("/client-home"),
        vec![
            ("HTTPS_PROXY".into(), "http://client-proxy:3128".into()),
            ("HOME".into(), "/server-path".into()),
            ("GH_PAGER".into(), "unsafe-pager".into()),
        ],
    )
}

fn scheduler() -> ServerPullRequests {
    ServerPullRequests::new(
        RepositoryStatusPreferences::default(),
        GitHubCliStatus::SignedIn,
    )
}

#[test]
fn pull_request_lookup_runs_only_on_client_with_client_home() {
    let runner = FakeRunner::new([exit(0, LISTED)]);
    let mut scheduler = scheduler();
    let before = Instant::now();
    let tickets = scheduler
        .observe(
            target(repository(1, 1)),
            repository(1, 1),
            &facts("local"),
            before,
        )
        .unwrap();
    assert_eq!(tickets.len(), 1);

    let result = tickets[0]
        .run(Some(&lookup(&runner)), &GitHubHosts::default())
        .unwrap()
        .unwrap();
    assert_eq!(result.number, 520);
    let requests = runner.requests();
    let [request] = requests.as_slice() else {
        panic!("expected only one client GitHub CLI call")
    };
    assert_eq!(request.executable, Path::new("/client-tools/gh/bin/gh"));
    assert_eq!(request.directory, Path::new("/client-home"));
    assert_eq!(request.arguments, [
        "pr", "list", "--repo=github.com/base/app", "--head=published", "--state=open", "--limit=10",
        "--json=number,title,isDraft,url,headRefName,baseRefName,headRepositoryOwner,isCrossRepository",
    ].map(std::ffi::OsString::from));
    let environment = |name: &str| {
        request
            .environment
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_os_str())
    };
    assert_eq!(
        environment("HOME"),
        Some(Path::new("/client-home").as_os_str())
    );
    assert_eq!(
        environment("HTTPS_PROXY"),
        Some(std::ffi::OsStr::new("http://client-proxy:3128"))
    );
    assert_eq!(environment("GH_PAGER"), Some(std::ffi::OsStr::new("cat")));
    assert_eq!(
        environment("GH_PROMPT_DISABLED"),
        Some(std::ffi::OsStr::new("1"))
    );
    assert_eq!(request.stdout_limit, Some(PULL_REQUEST_OUTPUT_LIMIT));
    assert!(request.deadline.unwrap() >= before + PULL_REQUEST_TIMEOUT);
    assert!(request.deadline.unwrap() <= Instant::now() + PULL_REQUEST_TIMEOUT);
    assert!(!request.keep_process_group_on_exit);
    assert_eq!(tickets[0].repository(), repository(1, 1));
    scheduler
        .finished(tickets[0].clone(), Ok(Some(result)), Instant::now())
        .unwrap();
    assert_eq!(
        scheduler.view(target(repository(1, 1))).unwrap().number,
        520
    );
}

fn one(tickets: Vec<ServerPullRequestTicket>) -> ServerPullRequestTicket {
    assert_eq!(tickets.len(), 1);
    tickets.into_iter().next().unwrap()
}

fn pull_request(number: u32) -> PullRequest {
    PullRequest {
        number,
        title: "Client-local result".into(),
        draft: false,
        url: format!("https://github.com/base/app/pull/{number}").into(),
        head: "published".into(),
        base: "main".into(),
    }
}

#[test]
fn branch_upstream_and_window_activation_trigger_but_status_revisions_do_not() {
    use spaceterm_core::worktrees::queries::{GitDivergence, GitUpstream};
    let mut scheduler = scheduler();
    let repository = repository(1, 1);
    let now = Instant::now();
    let mut status = facts("local");
    let ticket = one(scheduler
        .observe(target(repository), repository, &status, now)
        .unwrap());
    assert!(
        scheduler
            .finished(ticket, Ok(None), now)
            .unwrap()
            .is_empty()
    );

    status.revision = NonZeroU64::new(2).unwrap();
    status.dirty = true;
    assert!(
        scheduler
            .observe(target(repository), repository, &status, now)
            .unwrap()
            .is_empty()
    );
    status.upstream = Some(GitUpstream {
        name: BranchName::new("origin/local".into()).unwrap(),
        divergence: Some(GitDivergence {
            ahead: 1,
            behind: 0,
        }),
    });
    let ticket = one(scheduler
        .observe(target(repository), repository, &status, now)
        .unwrap());
    scheduler.finished(ticket, Ok(None), now).unwrap();
    status
        .upstream
        .as_mut()
        .unwrap()
        .divergence
        .as_mut()
        .unwrap()
        .ahead = 2;
    let ticket = one(scheduler
        .observe(target(repository), repository, &status, now)
        .unwrap());
    scheduler.finished(ticket, Ok(None), now).unwrap();
    status.branch = Some(BranchName::new("other-local".into()).unwrap());
    let ticket = one(scheduler
        .observe(target(repository), repository, &status, now)
        .unwrap());
    scheduler.finished(ticket, Ok(None), now).unwrap();
    assert!(scheduler.window_activated([], now).unwrap().is_empty());
    let ticket = one(scheduler
        .window_activated([target(repository), target(repository)], now)
        .unwrap());
    assert!(
        scheduler
            .finished(ticket, Ok(None), now)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn repeated_refreshes_coalesce_to_one_followup_and_failures_keep_the_last_result() {
    let mut scheduler = scheduler();
    let repository = repository(1, 1);
    let now = Instant::now();
    let later = now + Duration::from_millis(1);
    let ticket = one(scheduler
        .observe(target(repository), repository, &facts("local"), now)
        .unwrap());
    assert!(
        scheduler
            .window_activated([target(repository)], now)
            .unwrap()
            .is_empty()
    );
    for _ in 0..100 {
        assert!(
            scheduler
                .window_activated([target(repository)], later)
                .unwrap()
                .is_empty()
        );
    }
    let followup = one(scheduler
        .finished(ticket.clone(), Ok(Some(pull_request(1))), later)
        .unwrap());
    assert_eq!(scheduler.view(target(repository)).unwrap().number, 1);
    assert!(
        scheduler
            .finished(ticket, Ok(Some(pull_request(999))), later)
            .unwrap()
            .is_empty()
    );
    assert_eq!(scheduler.view(target(repository)).unwrap().number, 1);
    assert!(
        scheduler
            .finished(followup, Err(PullRequestError::Unavailable), later)
            .unwrap()
            .is_empty()
    );
    assert_eq!(scheduler.view(target(repository)).unwrap().number, 1);
}

#[test]
fn equal_repository_and_branch_display_names_are_isolated_across_servers() {
    let mut scheduler = scheduler();
    let now = Instant::now();
    let first = repository(1, 1);
    let second = repository(2, 1);
    let first_ticket = one(scheduler
        .observe(target(first), first, &facts("local"), now)
        .unwrap());
    let second_ticket = one(scheduler
        .observe(target(second), second, &facts("local"), now)
        .unwrap());
    scheduler
        .finished(second_ticket, Ok(Some(pull_request(2))), now)
        .unwrap();
    scheduler
        .finished(first_ticket, Ok(Some(pull_request(1))), now)
        .unwrap();
    assert_eq!(scheduler.view(target(first)).unwrap().number, 1);
    assert_eq!(scheduler.view(target(second)).unwrap().number, 2);
    scheduler.remove(target(first), now).unwrap();
    assert!(scheduler.view(target(first)).is_none());
    assert_eq!(scheduler.view(target(second)).unwrap().number, 2);
}

#[test]
fn changing_branch_cancels_old_work_and_restores_only_the_matching_cached_branch() {
    let mut scheduler = scheduler();
    let repository = repository(1, 1);
    let now = Instant::now();
    let first = one(scheduler
        .observe(target(repository), repository, &facts("first"), now)
        .unwrap());
    scheduler
        .finished(first, Ok(Some(pull_request(1))), now)
        .unwrap();
    let refresh = one(scheduler
        .window_activated([target(repository)], now)
        .unwrap());
    let second = one(scheduler
        .observe(target(repository), repository, &facts("second"), now)
        .unwrap());
    assert!(refresh.is_cancelled());
    assert!(scheduler.view(target(repository)).is_none());
    scheduler
        .finished(second, Ok(Some(pull_request(2))), now)
        .unwrap();
    scheduler
        .finished(refresh, Ok(Some(pull_request(999))), now)
        .unwrap();
    assert_eq!(scheduler.view(target(repository)).unwrap().number, 2);
    let first = one(scheduler
        .observe(target(repository), repository, &facts("first"), now)
        .unwrap());
    assert_eq!(scheduler.view(target(repository)).unwrap().number, 1);
    scheduler.finished(first, Ok(None), now).unwrap();
    assert!(scheduler.view(target(repository)).is_none());
}

#[test]
fn changed_normalized_query_invalidates_a_same_branch_result_and_its_generation() {
    let mut scheduler = scheduler();
    let repository = repository(1, 1);
    let now = Instant::now();
    let status = facts("local");
    let first = one(scheduler
        .observe(target(repository), repository, &status, now)
        .unwrap());
    scheduler
        .finished(first, Ok(Some(pull_request(1))), now)
        .unwrap();
    let obsolete = one(scheduler
        .window_activated([target(repository)], now)
        .unwrap());
    let mut changed = status.clone();
    changed.pull_request = Some(
        GitPullRequestFacts::new(
            "github.com".into(),
            "different-base".into(),
            "app".into(),
            BranchName::new("different-push-branch".into()).unwrap(),
            Some("other-fork".into()),
        )
        .unwrap(),
    );
    let current = one(scheduler
        .observe(target(repository), repository, &changed, now)
        .unwrap());
    assert!(obsolete.is_cancelled());
    assert!(scheduler.view(target(repository)).is_none());
    scheduler
        .finished(obsolete, Err(PullRequestError::ToolMissing), now)
        .unwrap();
    scheduler
        .finished(current, Ok(Some(pull_request(2))), now)
        .unwrap();
    assert_eq!(scheduler.view(target(repository)).unwrap().number, 2);
}

#[test]
fn removal_and_same_key_reappearance_reject_old_and_duplicate_completions() {
    let mut scheduler = scheduler();
    let repository = repository(1, 1);
    let now = Instant::now();
    let stale = one(scheduler
        .observe(target(repository), repository, &facts("local"), now)
        .unwrap());
    scheduler.remove(target(repository), now).unwrap();
    assert!(stale.is_cancelled());
    let current = one(scheduler
        .observe(target(repository), repository, &facts("local"), now)
        .unwrap());
    scheduler
        .finished(stale.clone(), Ok(Some(pull_request(999))), now)
        .unwrap();
    assert!(scheduler.view(target(repository)).is_none());
    scheduler
        .finished(current, Ok(Some(pull_request(1))), now)
        .unwrap();
    scheduler
        .finished(stale, Ok(Some(pull_request(999))), now)
        .unwrap();
    assert_eq!(scheduler.view(target(repository)).unwrap().number, 1);
}

#[test]
fn absent_query_or_detached_head_cancels_lookup_and_clears_presentation() {
    for detached in [false, true] {
        let mut scheduler = scheduler();
        let repository = repository(1, 1);
        let now = Instant::now();
        let ticket = one(scheduler
            .observe(target(repository), repository, &facts("local"), now)
            .unwrap());
        scheduler
            .finished(ticket, Ok(Some(pull_request(1))), now)
            .unwrap();
        let pending = one(scheduler
            .window_activated([target(repository)], now)
            .unwrap());
        let mut status = facts("local");
        if detached {
            status.branch = None;
        } else {
            status.pull_request = None;
        }
        assert!(
            scheduler
                .observe(target(repository), repository, &status, now)
                .unwrap()
                .is_empty()
        );
        assert!(pending.is_cancelled());
        scheduler
            .finished(pending, Ok(Some(pull_request(999))), now)
            .unwrap();
        assert!(scheduler.view(target(repository)).is_none());
        assert!(
            scheduler
                .window_activated([target(repository)], now)
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn client_preferences_and_tool_policy_cancel_work_and_require_explicit_reenable() {
    let now = Instant::now();
    let repository = repository(1, 1);
    for (preferences, tool) in [
        (
            RepositoryStatusPreferences {
                show_repository_status: true,
                show_pull_requests: false,
            },
            GitHubCliStatus::SignedIn,
        ),
        (
            RepositoryStatusPreferences {
                show_repository_status: false,
                show_pull_requests: true,
            },
            GitHubCliStatus::SignedIn,
        ),
        (
            RepositoryStatusPreferences::default(),
            GitHubCliStatus::SignedOut,
        ),
        (
            RepositoryStatusPreferences::default(),
            GitHubCliStatus::Unknown,
        ),
        (
            RepositoryStatusPreferences::default(),
            GitHubCliStatus::NotFound,
        ),
        (
            RepositoryStatusPreferences::default(),
            GitHubCliStatus::Unavailable,
        ),
    ] {
        let mut scheduler = scheduler();
        let initial = one(scheduler
            .observe(target(repository), repository, &facts("local"), now)
            .unwrap());
        scheduler
            .finished(initial, Ok(Some(pull_request(1))), now)
            .unwrap();
        let pending = one(scheduler
            .window_activated([target(repository)], now)
            .unwrap());
        assert!(
            scheduler
                .set_policy(preferences, tool, now)
                .unwrap()
                .is_empty()
        );
        assert!(pending.is_cancelled());
        scheduler
            .finished(pending, Ok(Some(pull_request(999))), now)
            .unwrap();
        assert!(scheduler.view(target(repository)).is_none());
        assert!(
            scheduler
                .observe(target(repository), repository, &facts("local"), now)
                .unwrap()
                .is_empty()
        );
        assert!(
            scheduler
                .window_activated([target(repository)], now)
                .unwrap()
                .is_empty()
        );
        let enabled = one(scheduler
            .set_policy(
                RepositoryStatusPreferences::default(),
                GitHubCliStatus::SignedIn,
                now,
            )
            .unwrap());
        scheduler
            .finished(enabled, Ok(Some(pull_request(2))), now)
            .unwrap();
        assert_eq!(scheduler.view(target(repository)).unwrap().number, 2);
    }
}

#[test]
fn signed_out_lookup_clears_only_its_branch_while_missing_tool_clears_every_result() {
    let mut scheduler = scheduler();
    let now = Instant::now();
    let first = repository(1, 1);
    let second = repository(2, 1);
    for (repository, number) in [(first, 1), (second, 2)] {
        let ticket = one(scheduler
            .observe(target(repository), repository, &facts("local"), now)
            .unwrap());
        scheduler
            .finished(ticket, Ok(Some(pull_request(number))), now)
            .unwrap();
    }
    let ticket = one(scheduler.window_activated([target(first)], now).unwrap());
    scheduler
        .finished(ticket, Err(PullRequestError::NotLoggedIn), now)
        .unwrap();
    assert!(scheduler.view(target(first)).is_none());
    assert_eq!(scheduler.view(target(second)).unwrap().number, 2);
    let ticket = one(scheduler.window_activated([target(first)], now).unwrap());
    scheduler
        .finished(ticket, Err(PullRequestError::ToolMissing), now)
        .unwrap();
    assert!(scheduler.view(target(second)).is_none());
    assert!(
        scheduler
            .window_activated([target(first), target(second)], now)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn cancelled_workers_hold_the_four_worker_slots_until_their_completions_arrive() {
    let mut scheduler = scheduler();
    let now = Instant::now();
    let mut running = Vec::new();
    for index in 1..=8 {
        let tickets = scheduler
            .observe(
                target(repository(1, index)),
                repository(1, index),
                &facts("local"),
                now,
            )
            .unwrap();
        if index <= 4 {
            running.push(one(tickets));
        } else {
            assert!(tickets.is_empty());
        }
    }
    for index in 1..=4 {
        assert!(
            scheduler
                .remove(target(repository(1, index)), now)
                .unwrap()
                .is_empty()
        );
    }
    for (index, ticket) in running.into_iter().enumerate() {
        assert!(ticket.is_cancelled());
        let replacement = one(scheduler
            .finished(ticket, Err(PullRequestError::Cancelled), now)
            .unwrap());
        assert_eq!(replacement.repository(), repository(1, index as u64 + 5));
    }
}

#[test]
fn ready_repository_does_not_starve_behind_a_running_repository_followup() {
    let mut scheduler = scheduler();
    let now = Instant::now();
    let mut running = Vec::new();
    for index in 1..=5 {
        let tickets = scheduler
            .observe(
                target(repository(1, index)),
                repository(1, index),
                &facts("local"),
                now,
            )
            .unwrap();
        if index <= 4 {
            running.push(one(tickets));
        } else {
            assert!(tickets.is_empty());
        }
    }
    scheduler
        .window_activated([target(repository(1, 1))], now + Duration::from_millis(1))
        .unwrap();
    let next = one(scheduler
        .finished(running.remove(0), Ok(None), now)
        .unwrap());
    assert_eq!(next.repository(), repository(1, 5));
}

#[test]
fn activation_batch_preserves_an_already_queued_repositorys_position() {
    let mut scheduler = scheduler();
    let now = Instant::now();
    let later = now + Duration::from_millis(1);
    let mut running = Vec::new();
    for index in 1..=5 {
        let tickets = scheduler
            .observe(
                target(repository(1, index)),
                repository(1, index),
                &facts("local"),
                now,
            )
            .unwrap();
        if index <= 4 {
            running.push(one(tickets));
        } else {
            assert!(tickets.is_empty());
        }
    }
    for _ in 0..3 {
        assert!(
            scheduler
                .window_activated([target(repository(1, 1)), target(repository(1, 5))], later)
                .unwrap()
                .is_empty()
        );
    }
    let next = one(scheduler
        .finished(running.remove(0), Ok(None), later)
        .unwrap());
    assert_eq!(next.repository(), repository(1, 5));
}

#[test]
fn unchanged_status_does_not_move_an_already_queued_repository_behind_new_work() {
    let mut scheduler = scheduler();
    let now = Instant::now();
    let mut running = Vec::new();
    for index in 1..=6 {
        let tickets = scheduler
            .observe(
                target(repository(1, index)),
                repository(1, index),
                &facts("local"),
                now,
            )
            .unwrap();
        if index <= 4 {
            running.push(one(tickets));
        } else {
            assert!(tickets.is_empty());
        }
    }
    assert!(
        scheduler
            .observe(
                target(repository(1, 5)),
                repository(1, 5),
                &facts("local"),
                now
            )
            .unwrap()
            .is_empty()
    );
    let next = one(scheduler
        .finished(running.remove(0), Ok(None), now)
        .unwrap());
    assert_eq!(next.repository(), repository(1, 5));
}

#[test]
fn status_interest_admission_is_bounded_without_discarding_existing_interests() {
    let mut scheduler = scheduler();
    let now = Instant::now();
    let mut first = None;
    for index in 1..=256 {
        let tickets = scheduler
            .observe(
                target(repository(1, index)),
                repository(1, index),
                &facts("local"),
                now,
            )
            .unwrap();
        if index == 1 {
            first = Some(one(tickets));
        }
    }
    assert!(
        scheduler
            .observe(
                target(repository(1, 257)),
                repository(1, 257),
                &facts("local"),
                now
            )
            .is_err()
    );
    scheduler
        .finished(first.unwrap(), Ok(Some(pull_request(1))), now)
        .unwrap();
    assert_eq!(scheduler.view(target(repository(1, 1))).unwrap().number, 1);
    scheduler.remove(target(repository(1, 256)), now).unwrap();
    assert!(
        scheduler
            .observe(
                target(repository(1, 257)),
                repository(1, 257),
                &facts("local"),
                now
            )
            .unwrap()
            .is_empty()
    );
}

#[test]
fn old_branch_cache_is_bounded_and_never_evicts_another_repositories_current_result() {
    let mut scheduler = scheduler();
    let now = Instant::now();
    let first = repository(1, 1);
    let second = repository(1, 2);
    let ticket = one(scheduler
        .observe(target(first), first, &facts("kept"), now)
        .unwrap());
    scheduler
        .finished(ticket, Ok(Some(pull_request(1))), now)
        .unwrap();
    for index in 0..300 {
        let ticket = one(scheduler
            .observe(
                target(second),
                second,
                &facts(&format!("branch-{index}")),
                now,
            )
            .unwrap());
        scheduler
            .finished(ticket, Ok(Some(pull_request(index + 2))), now)
            .unwrap();
    }
    let ticket = one(scheduler
        .observe(target(second), second, &facts("branch-0"), now)
        .unwrap());
    assert!(scheduler.view(target(second)).is_none());
    assert_eq!(scheduler.view(target(first)).unwrap().number, 1);
    scheduler.finished(ticket, Ok(None), now).unwrap();
}

#[test]
fn normalized_server_remote_policy_matches_legacy_precedence_without_forwarding_credentials() {
    use crate::repository_status::pull_request::pull_request_query;
    use crate::repository_status::{RepositoryConfig, RepositoryHead};
    let now = Instant::now();
    for (has_upstream, has_branch_remote, push_remote, push_default, push_to_upstream) in [
        (true, true, true, true, true),
        (false, true, false, true, false),
        (false, true, false, false, true),
        (false, false, false, false, false),
    ] {
        let mut config = RepositoryConfig {
            remotes: vec![
                (
                    "origin".into(),
                    "https://private-user:private-value@GitHub.com/origin-owner/app.git".into(),
                ),
                (
                    "selected".into(),
                    "git@github.com:selected-owner/app.git".into(),
                ),
                (
                    "push".into(),
                    "ssh://private-user@github.com/push-owner/app.git".into(),
                ),
                (
                    "default-push".into(),
                    "git@github.com:default-owner/app.git".into(),
                ),
            ],
            branch_remote: has_branch_remote.then(|| "selected".into()),
            branch_push_remote: push_remote.then(|| "push".into()),
            push_default: push_default.then(|| "default-push".into()),
            push_to_upstream,
            branch_merge: Some("refs/heads/published".into()),
            ..RepositoryConfig::default()
        };
        if has_upstream {
            config
                .remotes
                .push(("upstream".into(), "git@github.com:base/app.git".into()));
        }
        let head = RepositoryHead::Branch("local".into());
        let expected = pull_request_query(&config, &head).unwrap();
        let normalized = spaceterm_host::git::server::pull_request_facts(&config, &head).unwrap();

        let mut status = facts("local");
        status.pull_request = Some(normalized.clone());
        let owner = if has_upstream {
            "base"
        } else if has_branch_remote {
            "selected-owner"
        } else {
            "origin-owner"
        };
        let published = if push_to_upstream {
            "published"
        } else {
            "local"
        };
        let head_owner = if push_remote {
            "push-owner"
        } else if push_default {
            "default-owner"
        } else {
            "selected-owner"
        };
        let response = serde_json::json!([{
            "number": 520, "title": "Retained remote selection", "isDraft": false,
            "url": format!("https://github.com/{owner}/app/pull/520"),
            "headRefName": published, "baseRefName": "main",
            "headRepositoryOwner": {"login": head_owner},
            "isCrossRepository": has_branch_remote,
        }])
        .to_string();
        let runner = FakeRunner::new([exit(0, &response)]);
        let mut scheduler = scheduler();
        let ticket = one(scheduler
            .observe(target(repository(1, 1)), repository(1, 1), &status, now)
            .unwrap());
        let result = ticket
            .run(Some(&lookup(&runner)), &GitHubHosts::default())
            .unwrap()
            .unwrap();
        assert_eq!(result.number, 520);
        let request = &runner.requests()[0];
        assert_eq!(
            request.arguments,
            crate::repository_status::pull_request::pull_request_arguments(&expected)
        );
        assert!(
            request
                .arguments
                .contains(&format!("--repo=github.com/{owner}/app").into())
        );
        assert!(
            request
                .arguments
                .contains(&format!("--head={published}").into())
        );
        let rendered = format!("{:?} {:?} {:?}", normalized, ticket, request.arguments);
        assert!(!rendered.contains("private-user") && !rendered.contains("private-value"));
        let original = status.clone();
        scheduler.finished(ticket, Ok(Some(result)), now).unwrap();
        assert_eq!(status, original);
    }
}

#[test]
fn enterprise_host_validation_retries_login_then_reuses_only_a_confirmed_host() {
    let now = Instant::now();
    let repository = repository(1, 1);
    let mut status = facts("local");
    status.pull_request = Some(
        GitPullRequestFacts::new(
            "git.example.com".into(),
            "base".into(),
            "app".into(),
            BranchName::new("published".into()).unwrap(),
            Some("fork".into()),
        )
        .unwrap(),
    );
    let listed = LISTED.replace("github.com", "git.example.com");
    let runner = FakeRunner::new([exit(4, ""), exit(0, ""), exit(0, &listed), exit(0, &listed)]);
    let lookup = lookup(&runner);
    let hosts = GitHubHosts::default();
    let mut scheduler = scheduler();
    let ticket = one(scheduler
        .observe(target(repository), repository, &status, now)
        .unwrap());
    let result = ticket.run(Some(&lookup), &hosts).unwrap();
    assert!(result.is_none());
    scheduler.finished(ticket, Ok(result), now).unwrap();
    for _ in 0..2 {
        let ticket = one(scheduler
            .window_activated([target(repository)], now)
            .unwrap());
        let result = ticket.run(Some(&lookup), &hosts).unwrap();
        assert_eq!(result.as_ref().unwrap().number, 520);
        scheduler.finished(ticket, Ok(result), now).unwrap();
    }
    let requests = runner.requests();
    assert_eq!(requests.len(), 4);
    for request in &requests[..2] {
        assert_eq!(
            request.arguments,
            ["auth", "status", "--hostname", "git.example.com"].map(std::ffi::OsString::from)
        );
    }
    assert!(
        requests
            .iter()
            .all(|request| request.directory == Path::new("/client-home"))
    );
}

#[test]
fn cancellation_before_execution_and_drop_never_spawn_a_client_tool() {
    let now = Instant::now();
    let repository = repository(1, 1);
    let mut scheduler = scheduler();
    let ticket = one(scheduler
        .observe(target(repository), repository, &facts("local"), now)
        .unwrap());
    drop(scheduler);
    let runner = FakeRunner::new([]);
    assert!(ticket.is_cancelled());
    assert_eq!(
        ticket.run(Some(&lookup(&runner)), &GitHubHosts::default()),
        Err(PullRequestError::Cancelled)
    );
    assert!(runner.requests().is_empty());
}

#[test]
fn cancellation_after_native_runner_completion_rejects_its_late_success() {
    use crate::repository_status::{ProgramError, ProgramExit, ProgramRequest};
    use std::sync::{Mutex, mpsc};
    struct DelayedRunner {
        started: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }
    impl RepositoryProgramRunner for DelayedRunner {
        fn run(
            &self,
            request: &ProgramRequest,
            stdout: &mut dyn FnMut(&[u8]),
            cancellation: &SshCancellationToken,
        ) -> Result<ProgramExit, ProgramError> {
            assert_eq!(request.directory, Path::new("/client-home"));
            self.started.send(()).unwrap();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
            assert!(cancellation.is_cancelled());
            stdout(LISTED.as_bytes());
            Ok(ProgramExit { code: Some(0) })
        }
    }
    let (started_sender, started) = mpsc::channel();
    let (release, receiver) = mpsc::channel();
    let lookup = GitHubCliLookup::new(
        Arc::new(DelayedRunner {
            started: started_sender,
            release: Mutex::new(receiver),
        }),
        PathBuf::from("/client-tools/gh/bin/gh"),
        PathBuf::from("/client-home"),
        Vec::new(),
    );
    let now = Instant::now();
    let repository = repository(1, 1);
    let mut scheduler = scheduler();
    let ticket = one(scheduler
        .observe(target(repository), repository, &facts("local"), now)
        .unwrap());
    let work = ticket.clone();
    let worker = std::thread::spawn(move || work.run(Some(&lookup), &GitHubHosts::default()));
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    scheduler.remove(target(repository), now).unwrap();
    release.send(()).unwrap();
    let result = worker.join().unwrap();
    assert_eq!(result, Err(PullRequestError::Cancelled));
    scheduler.finished(ticket, result, now).unwrap();
    assert!(scheduler.view(target(repository)).is_none());
}

#[test]
fn replacing_the_cache_cannot_settle_a_new_request_with_an_old_ticket() {
    let now = Instant::now();
    let repository = repository(1, 1);
    let mut previous = scheduler();
    let old = one(previous
        .observe(target(repository), repository, &facts("local"), now)
        .unwrap());
    drop(previous);
    let mut current = scheduler();
    let new = one(current
        .observe(target(repository), repository, &facts("local"), now)
        .unwrap());
    current
        .finished(old, Ok(Some(pull_request(999))), now)
        .unwrap();
    current
        .finished(new, Ok(Some(pull_request(1))), now)
        .unwrap();
    assert_eq!(current.view(target(repository)).unwrap().number, 1);
}

#[test]
fn a_completed_branch_cache_survives_detached_head_without_lookup_or_presentation() {
    let now = Instant::now();
    let repository = repository(1, 1);
    let mut scheduler = scheduler();
    let ticket = one(scheduler
        .observe(target(repository), repository, &facts("local"), now)
        .unwrap());
    scheduler
        .finished(ticket, Ok(Some(pull_request(1))), now)
        .unwrap();
    let mut detached = facts("local");
    detached.branch = None;
    detached.pull_request = None;
    assert!(
        scheduler
            .observe(target(repository), repository, &detached, now)
            .unwrap()
            .is_empty()
    );
    assert!(scheduler.view(target(repository)).is_none());
    assert!(
        scheduler
            .window_activated([target(repository)], now)
            .unwrap()
            .is_empty()
    );
    let refresh = one(scheduler
        .observe(target(repository), repository, &facts("local"), now)
        .unwrap());
    assert_eq!(scheduler.view(target(repository)).unwrap().number, 1);
    scheduler.finished(refresh, Ok(None), now).unwrap();
    assert!(scheduler.view(target(repository)).is_none());
}

#[test]
fn concurrent_worktree_branches_share_a_repository_without_cancelling_each_other() {
    let mut scheduler = scheduler();
    let repository = repository(1, 1);
    let first_target = GitStatusTarget::Worktree(WorktreeId::new(repository.server(), 1).unwrap());
    let second_target = GitStatusTarget::Worktree(WorktreeId::new(repository.server(), 2).unwrap());
    let now = Instant::now();
    let first = one(scheduler
        .observe(first_target, repository, &facts("first"), now)
        .unwrap());
    let second = one(scheduler
        .observe(second_target, repository, &facts("second"), now)
        .unwrap());
    assert!(
        !first.is_cancelled(),
        "another Worktree must not cancel this branch"
    );
    assert!(!second.is_cancelled());
    let later = now + Duration::from_millis(1);
    for _ in 0..3 {
        assert!(
            scheduler
                .observe(first_target, repository, &facts("first"), later)
                .unwrap()
                .is_empty()
        );
        assert!(
            scheduler
                .observe(second_target, repository, &facts("second"), later)
                .unwrap()
                .is_empty()
        );
    }
    assert!(!first.is_cancelled() && !second.is_cancelled());
    scheduler
        .finished(first, Ok(Some(pull_request(1))), now)
        .unwrap();
    assert_eq!(scheduler.view(first_target).unwrap().number, 1);
    assert!(scheduler.view(second_target).is_none());
    scheduler
        .finished(second, Ok(Some(pull_request(2))), now)
        .unwrap();
    assert_eq!(scheduler.view(first_target).unwrap().number, 1);
    assert_eq!(scheduler.view(second_target).unwrap().number, 2);
    let refresh = one(scheduler
        .window_activated([first_target, first_target], later)
        .unwrap());
    assert!(
        scheduler
            .finished(refresh, Ok(Some(pull_request(3))), later)
            .unwrap()
            .is_empty()
    );
    assert_eq!(scheduler.view(first_target).unwrap().number, 3);
    assert_eq!(scheduler.view(second_target).unwrap().number, 2);
}

#[test]
fn same_branch_targets_with_different_normalized_queries_remain_independent() {
    let repository = repository(1, 1);
    let first_target = target(repository);
    let second_target = GitStatusTarget::Worktree(WorktreeId::new(repository.server(), 2).unwrap());
    let now = Instant::now();
    let later = now + Duration::from_millis(1);
    let first_status = facts("local");
    // Worktree-local merge and push-remote settings can differ even on the same local branch.
    for (host, owner, name, head_branch, head_owner) in [
        ("git.example.com", "base", "app", "published", Some("fork")),
        ("github.com", "other-base", "app", "published", Some("fork")),
        ("github.com", "base", "other-app", "published", Some("fork")),
        ("github.com", "base", "app", "other-published", Some("fork")),
        ("github.com", "base", "app", "published", Some("other-fork")),
        ("github.com", "base", "app", "published", None),
    ] {
        let mut scheduler = scheduler();
        let mut second_status = first_status.clone();
        second_status.pull_request = Some(
            GitPullRequestFacts::new(
                host.into(),
                owner.into(),
                name.into(),
                BranchName::new(head_branch.into()).unwrap(),
                head_owner.map(Into::into),
            )
            .unwrap(),
        );
        let first = one(scheduler
            .observe(first_target, repository, &first_status, now)
            .unwrap());
        let second = one(scheduler
            .observe(second_target, repository, &second_status, now)
            .unwrap());
        assert!(
            !first.is_cancelled(),
            "another Worktree's normalized query must not cancel this lookup"
        );
        for _ in 0..3 {
            for (target, status) in [
                (first_target, &first_status),
                (second_target, &second_status),
            ] {
                assert!(
                    scheduler
                        .observe(target, repository, status, later)
                        .unwrap()
                        .is_empty()
                );
            }
        }
        assert!(!first.is_cancelled() && !second.is_cancelled());
        assert!(
            scheduler
                .finished(second, Ok(Some(pull_request(2))), later)
                .unwrap()
                .is_empty()
        );
        assert!(scheduler.view(first_target).is_none());
        assert_eq!(scheduler.view(second_target).unwrap().number, 2);
        assert!(
            scheduler
                .finished(first, Ok(Some(pull_request(1))), later)
                .unwrap()
                .is_empty()
        );
        for _ in 0..3 {
            for (target, status) in [
                (first_target, &first_status),
                (second_target, &second_status),
            ] {
                assert!(
                    scheduler
                        .observe(target, repository, status, later)
                        .unwrap()
                        .is_empty()
                );
            }
        }
        assert_eq!(scheduler.view(first_target).unwrap().number, 1);
        assert_eq!(scheduler.view(second_target).unwrap().number, 2);
    }
}

#[test]
fn same_branch_query_rebinding_cancels_only_the_last_matching_interest() {
    let mut scheduler = scheduler();
    let repository = repository(1, 1);
    let first_target = target(repository);
    let second_target = GitStatusTarget::Worktree(WorktreeId::new(repository.server(), 2).unwrap());
    let now = Instant::now();
    let initial_status = facts("local");
    let mut changed_status = initial_status.clone();
    changed_status.pull_request = Some(
        GitPullRequestFacts::new(
            "github.com".into(),
            "base".into(),
            "app".into(),
            BranchName::new("other-published".into()).unwrap(),
            Some("fork".into()),
        )
        .unwrap(),
    );
    let original = one(scheduler
        .observe(first_target, repository, &initial_status, now)
        .unwrap());
    assert!(
        scheduler
            .observe(second_target, repository, &initial_status, now)
            .unwrap()
            .is_empty()
    );
    let changed = one(scheduler
        .observe(first_target, repository, &changed_status, now)
        .unwrap());
    assert!(!original.is_cancelled());
    assert!(!changed.is_cancelled());
    assert!(
        scheduler
            .observe(second_target, repository, &changed_status, now)
            .unwrap()
            .is_empty()
    );
    assert!(original.is_cancelled());
    assert!(!changed.is_cancelled());
    // A stale failure cannot disable the matching query that both targets now share.
    assert!(
        scheduler
            .finished(original, Err(PullRequestError::ToolMissing), now)
            .unwrap()
            .is_empty()
    );
    assert!(scheduler.view(first_target).is_none());
    assert!(scheduler.view(second_target).is_none());
    let runner = FakeRunner::new([exit(0, &LISTED.replace("published", "other-published"))]);
    let result = changed
        .run(Some(&lookup(&runner)), &GitHubHosts::default())
        .unwrap();
    assert!(
        runner.requests()[0]
            .arguments
            .contains(&"--head=other-published".into())
    );
    assert!(
        scheduler
            .finished(changed, Ok(result), now)
            .unwrap()
            .is_empty()
    );
    assert_eq!(scheduler.view(first_target).unwrap().number, 520);
    assert_eq!(scheduler.view(second_target).unwrap().number, 520);
    let refresh = one(scheduler
        .window_activated([first_target, second_target], now)
        .unwrap());
    assert!(scheduler.remove(first_target, now).unwrap().is_empty());
    assert!(!refresh.is_cancelled());
    assert!(scheduler.remove(second_target, now).unwrap().is_empty());
    assert!(refresh.is_cancelled());
    scheduler
        .finished(refresh, Ok(Some(pull_request(999))), now)
        .unwrap();
    assert!(scheduler.view(first_target).is_none());
    assert!(scheduler.view(second_target).is_none());
}

#[test]
fn rebinding_one_status_target_keeps_other_targets_on_the_original_branch() {
    let now = Instant::now();
    let original = repository(1, 1);
    let first = GitStatusTarget::Pane(PaneId::new(original.server(), 1).unwrap());
    let second = GitStatusTarget::Workspace(WorkspaceId::new(original.server(), 1).unwrap());
    for (changed_repository, changed_branch) in [(original, "second"), (repository(1, 2), "first")]
    {
        let mut scheduler = scheduler();
        let shared = one(scheduler
            .observe(first, original, &facts("first"), now)
            .unwrap());
        assert!(
            scheduler
                .observe(second, original, &facts("first"), now)
                .unwrap()
                .is_empty()
        );
        let changed = one(scheduler
            .observe(first, changed_repository, &facts(changed_branch), now)
            .unwrap());
        assert!(!shared.is_cancelled());
        scheduler
            .finished(shared, Ok(Some(pull_request(1))), now)
            .unwrap();
        assert!(scheduler.view(first).is_none());
        assert_eq!(scheduler.view(second).unwrap().number, 1);
        scheduler
            .finished(changed, Ok(Some(pull_request(2))), now)
            .unwrap();
        assert_eq!(scheduler.view(first).unwrap().number, 2);
        assert_eq!(scheduler.view(second).unwrap().number, 1);
    }
}

#[test]
fn shared_branch_lookup_is_cancelled_only_after_its_last_status_target_leaves() {
    let mut scheduler = scheduler();
    let repository = repository(1, 1);
    let first = GitStatusTarget::Pane(PaneId::new(repository.server(), 1).unwrap());
    let second = GitStatusTarget::Workspace(WorkspaceId::new(repository.server(), 1).unwrap());
    let now = Instant::now();
    let shared = one(scheduler
        .observe(first, repository, &facts("local"), now)
        .unwrap());
    assert!(
        scheduler
            .observe(second, repository, &facts("local"), now)
            .unwrap()
            .is_empty()
    );
    assert!(scheduler.remove(first, now).unwrap().is_empty());
    assert!(!shared.is_cancelled());
    scheduler
        .finished(shared, Ok(Some(pull_request(1))), now)
        .unwrap();
    assert!(scheduler.view(first).is_none());
    assert_eq!(scheduler.view(second).unwrap().number, 1);
    let refresh = one(scheduler.window_activated([second], now).unwrap());
    assert!(scheduler.remove(second, now).unwrap().is_empty());
    assert!(refresh.is_cancelled());
    scheduler
        .finished(refresh, Ok(Some(pull_request(999))), now)
        .unwrap();
    assert!(scheduler.view(second).is_none());
    assert!(
        scheduler
            .window_activated([first, second], now)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn absent_query_or_detached_head_affects_only_its_own_status_target() {
    let repository = repository(1, 1);
    let first = target(repository);
    let second = GitStatusTarget::Worktree(WorktreeId::new(repository.server(), 2).unwrap());
    let now = Instant::now();
    for detached in [false, true] {
        let mut scheduler = scheduler();
        let shared = one(scheduler
            .observe(first, repository, &facts("local"), now)
            .unwrap());
        assert!(
            scheduler
                .observe(second, repository, &facts("local"), now)
                .unwrap()
                .is_empty()
        );
        let mut missing = facts("local");
        missing.pull_request = None;
        if detached {
            missing.branch = None;
        }
        assert!(
            scheduler
                .observe(first, repository, &missing, now)
                .unwrap()
                .is_empty()
        );
        assert!(!shared.is_cancelled());
        scheduler
            .finished(shared, Ok(Some(pull_request(1))), now)
            .unwrap();
        assert!(scheduler.view(first).is_none());
        assert_eq!(scheduler.view(second).unwrap().number, 1);
        assert!(scheduler.window_activated([first], now).unwrap().is_empty());
        let refresh = one(scheduler.window_activated([second], now).unwrap());
        scheduler
            .finished(refresh, Ok(Some(pull_request(2))), now)
            .unwrap();
        assert!(scheduler.view(first).is_none());
        assert_eq!(scheduler.view(second).unwrap().number, 2);
    }
}

#[test]
fn mismatched_status_target_and_repository_servers_are_rejected_without_rebinding() {
    let mut scheduler = scheduler();
    let first_repository = repository(1, 1);
    let interest = target(first_repository);
    let now = Instant::now();
    let ticket = one(scheduler
        .observe(interest, first_repository, &facts("local"), now)
        .unwrap());
    assert_eq!(
        scheduler
            .observe(interest, repository(2, 1), &facts("local"), now)
            .unwrap_err(),
        ServerPullRequestFailure::WrongServer
    );
    assert!(!ticket.is_cancelled());
    scheduler
        .finished(ticket, Ok(Some(pull_request(1))), now)
        .unwrap();
    assert_eq!(scheduler.view(interest).unwrap().number, 1);
}
