//! GitHub repositories named by git remote URLs.
//!
//! Remote URLs come from repository configuration, which anyone who wrote the repository controls.
//! Only network URL forms with a plain host and an `owner/name` path are accepted.

use std::sync::Arc;

use super::GitHubRepository;

const NETWORK_SCHEMES: [&str; 4] = ["https", "http", "ssh", "git"];

/// Whether a host is GitHub without asking the GitHub CLI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GitHubHost {
    /// `github.com` or a GitHub Enterprise Cloud `*.ghe.com` host.
    Known,
    /// Any other host is GitHub only when `gh auth status --hostname` succeeds for it.
    Unverified,
}

pub(crate) fn github_host(host: &str) -> GitHubHost {
    let enterprise_cloud = host
        .strip_suffix(".ghe.com")
        .is_some_and(|tenant| !tenant.is_empty());
    if host == "github.com" || enterprise_cloud {
        GitHubHost::Known
    } else {
        GitHubHost::Unverified
    }
}

/// Parses `https://`, `http://`, `ssh://`, and `git://` URLs and scp-like `user@host:owner/name`
/// remotes. Userinfo, the port, a trailing `/`, and `.git` are removed, and `ssh.github.com` maps to
/// `github.com`. Local paths, `file://`, transport helpers such as `ext::`, and anything with
/// control characters or whitespace are rejected.
pub(crate) fn parse_remote_url(url: &str) -> Option<GitHubRepository> {
    if url
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
    {
        return None;
    }
    let (authority, path) = match url.split_once("://") {
        Some((scheme, rest)) => {
            if !NETWORK_SCHEMES
                .iter()
                .any(|accepted| scheme.eq_ignore_ascii_case(accepted))
            {
                return None;
            }
            let (authority, path) = rest.split_once('/')?;
            (url_host(authority)?, path)
        }
        None => {
            if url.contains("::") {
                return None;
            }
            // Git reads `host:path` as scp-like only when the colon precedes every slash.
            let (authority, path) = url.split_once(':')?;
            if authority.contains('/') {
                return None;
            }
            let host = match authority.rsplit_once('@') {
                Some((user, host)) if !user.is_empty() => host,
                Some(_) => return None,
                None => authority,
            };
            (host, path.strip_prefix('/').unwrap_or(path))
        }
    };
    let host = host_name(authority)?;
    let (owner, name) = owner_and_name(path)?;
    Some(GitHubRepository {
        host,
        owner: owner.into(),
        name: name.into(),
    })
}

/// The host of a URL authority, without userinfo or a numeric port.
fn url_host(authority: &str) -> Option<&str> {
    let host_and_port = match authority.rsplit_once('@') {
        Some((user, host)) if !user.is_empty() => host,
        Some(_) => return None,
        None => authority,
    };
    match host_and_port.split_once(':') {
        Some((host, port))
            if !port.is_empty() && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit()) =>
        {
            Some(host)
        }
        Some(_) => None,
        None => Some(host_and_port),
    }
}

/// A lowercase DNS host name. IP literals in brackets and empty labels are rejected.
fn host_name(host: &str) -> Option<Arc<str>> {
    let host = host.to_ascii_lowercase();
    let valid = !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        });
    if !valid {
        return None;
    }
    Some(if host == "ssh.github.com" {
        "github.com".into()
    } else {
        host.into()
    })
}

fn owner_and_name(path: &str) -> Option<(&str, &str)> {
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    (is_path_segment(owner) && is_path_segment(name)).then_some((owner, name))
}

fn is_path_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && !segment.starts_with('-')
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository(host: &str, owner: &str, name: &str) -> Option<GitHubRepository> {
        Some(GitHubRepository {
            host: host.into(),
            owner: owner.into(),
            name: name.into(),
        })
    }

    #[test]
    fn network_urls_should_parse_to_host_owner_and_name() {
        for (url, expected) in [
            (
                "https://github.com/sadiksaifi/SpaceTerm.git",
                repository("github.com", "sadiksaifi", "SpaceTerm"),
            ),
            (
                "https://github.com/sadiksaifi/SpaceTerm",
                repository("github.com", "sadiksaifi", "SpaceTerm"),
            ),
            (
                "https://github.com/sadiksaifi/SpaceTerm/",
                repository("github.com", "sadiksaifi", "SpaceTerm"),
            ),
            (
                "https://github.com/sadiksaifi/SpaceTerm.git/",
                repository("github.com", "sadiksaifi", "SpaceTerm"),
            ),
            (
                "http://github.com/owner/repo",
                repository("github.com", "owner", "repo"),
            ),
            (
                "HTTPS://GitHub.COM/Owner/Repo",
                repository("github.com", "Owner", "Repo"),
            ),
            (
                "https://token@github.com/owner/repo.git",
                repository("github.com", "owner", "repo"),
            ),
            (
                "https://user:secret@github.com:443/owner/repo",
                repository("github.com", "owner", "repo"),
            ),
            (
                "ssh://git@github.com/owner/repo.git",
                repository("github.com", "owner", "repo"),
            ),
            (
                "ssh://git@ssh.github.com:443/owner/repo.git",
                repository("github.com", "owner", "repo"),
            ),
            (
                "ssh://github.com/owner/repo",
                repository("github.com", "owner", "repo"),
            ),
            (
                "git://github.com/owner/repo.git",
                repository("github.com", "owner", "repo"),
            ),
            (
                "https://acme.ghe.com/team/service",
                repository("acme.ghe.com", "team", "service"),
            ),
            (
                "https://git.example.com:8443/team/my_repo.v2.git",
                repository("git.example.com", "team", "my_repo.v2"),
            ),
            (
                "https://github.com/owner/.github",
                repository("github.com", "owner", ".github"),
            ),
        ] {
            assert_eq!(parse_remote_url(url), expected, "{url}");
        }
    }

    #[test]
    fn scp_like_remotes_should_parse_to_host_owner_and_name() {
        for (url, expected) in [
            (
                "git@github.com:owner/repo.git",
                repository("github.com", "owner", "repo"),
            ),
            (
                "git@github.com:owner/repo",
                repository("github.com", "owner", "repo"),
            ),
            (
                "github.com:owner/repo.git",
                repository("github.com", "owner", "repo"),
            ),
            (
                "git@github.com:/owner/repo.git",
                repository("github.com", "owner", "repo"),
            ),
            (
                "org-123@ssh.github.com:owner/repo.git/",
                repository("github.com", "owner", "repo"),
            ),
            (
                "git@ghe.internal:team/service.git",
                repository("ghe.internal", "team", "service"),
            ),
        ] {
            assert_eq!(parse_remote_url(url), expected, "{url}");
        }
    }

    #[test]
    fn unsafe_or_unsupported_remotes_should_be_rejected() {
        for url in [
            "",
            "file:///srv/git/repo.git",
            "FILE:///srv/git/repo.git",
            "file://host/owner/repo",
            "ext::ssh -o ProxyCommand=evil github.com %S owner/repo",
            "ext::sh-c-evil",
            "fd::3",
            "persistent-https::github.com/owner/repo",
            "git+ssh://github.com/owner/repo",
            "ftp://github.com/owner/repo",
            "/srv/git/repo.git",
            "./owner/repo",
            "../repo",
            "~/repo.git",
            "C:\\repos\\owner\\repo",
            "repo",
            "owner/repo",
            "./host:owner/repo",
            "https://github.com/owner/repo\n",
            "https://github.com/own\u{1b}er/repo",
            "https://github.com/owner/re\u{7f}po",
            "git@github.com:owner/repo\u{0}",
            "https://github.com/owner name/repo",
            "https://github.com/owner",
            "https://github.com/owner/",
            "https://github.com/owner/repo/pulls",
            "https://github.com//repo",
            "https://github.com/owner/repo?tab=1",
            "https://github.com/owner/repo#readme",
            "https://github.com/owner/re%2Fpo",
            "https://github.com/../repo",
            "https://github.com/owner/..",
            "https://github.com/-owner/repo",
            "https://github.com/owner/-repo",
            "https://github.com/owner/.git",
            "https://github.com:/owner/repo",
            "https://github.com:port/owner/repo",
            "https://github.com:443443/owner/repo",
            "https://@github.com/owner/repo",
            "https:///owner/repo",
            "https://[::1]/owner/repo",
            "https://github..com/owner/repo",
            "https://github.com./owner/repo",
            "https://-github.com/owner/repo",
            "https://git_hub.com/owner/repo",
            "https://github.com",
            "@github.com:owner/repo",
            "git@:owner/repo",
            "github.com:owner",
            "github.com:owner/repo/extra",
        ] {
            assert_eq!(parse_remote_url(url), None, "{url:?}");
        }
    }

    #[test]
    fn known_github_hosts_should_not_need_a_github_cli_check() {
        for (host, expected) in [
            ("github.com", GitHubHost::Known),
            ("acme.ghe.com", GitHubHost::Known),
            ("a.b.ghe.com", GitHubHost::Known),
            ("ghe.com", GitHubHost::Unverified),
            (".ghe.com", GitHubHost::Unverified),
            ("github.example.com", GitHubHost::Unverified),
            ("gitlab.com", GitHubHost::Unverified),
            ("notgithub.com", GitHubHost::Unverified),
            ("github.com.evil.example", GitHubHost::Unverified),
        ] {
            assert_eq!(github_host(host), expected, "{host}");
        }
    }

    #[test]
    fn repository_debug_should_redact_names() {
        let repository = parse_remote_url("git@github.com:secret-owner/secret-repo").unwrap();

        assert!(!format!("{repository:?}").contains("secret"));
    }
}
