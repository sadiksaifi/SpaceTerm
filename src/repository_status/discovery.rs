//! Repository discovery, configuration, and `HEAD` facts from git's probe output.
//!
//! Every function here parses bytes a local or remote probe already produced, so both paths share
//! one classification.

use std::fmt;
use std::sync::Arc;

use super::tools::BUILTIN_FSMONITOR_GIT_VERSION;
use super::{
    FsmonitorPolicy, RepositoryConfig, RepositoryHead, RepositoryReadError, StatusHeaders,
    ToolVersion, Upstream,
};

/// The most remotes retained from configuration.
pub(crate) const MAXIMUM_REMOTES: usize = 32;
/// Characters of the commit id presented for a detached `HEAD` and the Commit row.
pub(crate) const SHORT_COMMIT_LENGTH: usize = 7;
/// The largest configuration record retained. Larger records are skipped.
const MAXIMUM_CONFIG_RECORD_BYTES: usize = 64 * 1024;

/// What `git rev-parse` found for a directory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Discovery {
    NotRepository,
    /// Bare, inside a git directory, or rooted at home while the directory is below home.
    Hidden,
    Repository(DiscoveredRepository),
}

/// A work tree repository's locations, in the queried machine's path spelling.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct DiscoveredRepository {
    pub(crate) toplevel: Arc<str>,
    pub(crate) git_directory: Arc<str>,
    /// The shared common directory, already joined to the queried directory when git printed it
    /// relative.
    pub(crate) common_directory: Arc<str>,
    /// The queried directory relative to `toplevel`, empty at the top level.
    pub(crate) prefix: Arc<str>,
}

impl fmt::Debug for DiscoveredRepository {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DiscoveredRepository(<redacted>)")
    }
}

/// Classifies the stdout of `git rev-parse --is-inside-git-dir --is-bare-repository
/// --absolute-git-dir --git-common-dir --show-toplevel --show-prefix` run in `directory`.
///
/// Inside a git directory or in a bare repository git fails after the first lines, so the
/// classification reads those lines first. Empty output means git found no repository. Paths must
/// be UTF-8 because later commands name them again.
pub(crate) fn parse_discovery(
    output: &[u8],
    directory: &str,
    physical_home: &str,
) -> Result<Discovery, RepositoryReadError> {
    if output.is_empty() {
        return Ok(Discovery::NotRepository);
    }
    let output = std::str::from_utf8(output).map_err(|_| RepositoryReadError::InvalidResponse)?;
    let mut lines = output.split('\n');
    // Inside a git directory, git stops before reporting whether the repository is bare.
    if boolean(lines.next())? || boolean(lines.next())? {
        return Ok(Discovery::Hidden);
    }
    let mut path = || match lines.next() {
        Some(line) if !line.is_empty() => Ok(line),
        _ => Err(RepositoryReadError::InvalidResponse),
    };
    let git_directory = path()?;
    let common_directory = path()?;
    let toplevel = path()?;
    let prefix = lines.next().ok_or(RepositoryReadError::InvalidResponse)?;
    if lines.next() != Some("") || lines.next().is_some() {
        return Err(RepositoryReadError::InvalidResponse);
    }
    if is_hidden_home(toplevel, prefix, physical_home) {
        return Ok(Discovery::Hidden);
    }
    Ok(Discovery::Repository(DiscoveredRepository {
        toplevel: toplevel.into(),
        git_directory: git_directory.into(),
        common_directory: join_directory(directory, common_directory).into(),
        prefix: prefix.into(),
    }))
}

fn boolean(line: Option<&str>) -> Result<bool, RepositoryReadError> {
    match line {
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        _ => Err(RepositoryReadError::InvalidResponse),
    }
}

/// A repository rooted at home shows only for home itself, so a dotfiles repository does not
/// label every directory below home.
fn is_hidden_home(toplevel: &str, prefix: &str, physical_home: &str) -> bool {
    !prefix.is_empty() && trim_trailing_slashes(toplevel) == trim_trailing_slashes(physical_home)
}

fn trim_trailing_slashes(path: &str) -> &str {
    path.trim_end_matches('/')
}

fn join_directory(directory: &str, path: &str) -> String {
    if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("{}/{path}", trim_trailing_slashes(directory))
    }
}

/// Accepts `git config -z --get-regexp` output in chunks and keeps what Repository Status and
/// Pull Request lookup need for the current branch.
///
/// Each record is `key\nvalue\0`, or `key\0` for a key without a value, which git reads as a true
/// boolean. Later values replace earlier ones as git's own lookups do, except a remote keeps its
/// first URL, the one git fetches from.
pub(crate) struct ConfigParser {
    /// `branch.<current branch>.`, or `None` when `HEAD` is detached.
    branch_prefix: Option<Vec<u8>>,
    git_version: ToolVersion,
    config: RepositoryConfig,
    fsmonitor: bool,
    field: Vec<u8>,
    skipping: bool,
    failed: bool,
}

impl ConfigParser {
    pub(crate) fn new(branch: Option<&str>, git_version: ToolVersion) -> Self {
        Self {
            branch_prefix: branch.map(|branch| format!("branch.{branch}.").into_bytes()),
            git_version,
            config: RepositoryConfig::default(),
            fsmonitor: false,
            field: Vec::new(),
            skipping: false,
            failed: false,
        }
    }

    pub(crate) fn push(&mut self, mut chunk: &[u8]) {
        while !self.failed && !chunk.is_empty() {
            let end = chunk.iter().position(|byte| *byte == 0);
            let part = &chunk[..end.unwrap_or(chunk.len())];
            if !self.skipping {
                if self.field.len() + part.len() > MAXIMUM_CONFIG_RECORD_BYTES {
                    self.skipping = true;
                    self.field.clear();
                } else {
                    self.field.extend_from_slice(part);
                }
            }
            let Some(end) = end else {
                return;
            };
            if !std::mem::take(&mut self.skipping) {
                let field = std::mem::take(&mut self.field);
                self.record(&field);
                self.field = field;
                self.field.clear();
            }
            chunk = &chunk[end + 1..];
        }
    }

    pub(crate) fn finish(mut self) -> Result<RepositoryConfig, RepositoryReadError> {
        if self.failed || !self.field.is_empty() || self.skipping {
            return Err(RepositoryReadError::InvalidResponse);
        }
        if self.fsmonitor && self.git_version >= BUILTIN_FSMONITOR_GIT_VERSION {
            self.config.fsmonitor = FsmonitorPolicy::Builtin;
        }
        Ok(self.config)
    }

    fn record(&mut self, field: &[u8]) {
        let (key, value) = match field.iter().position(|byte| *byte == b'\n') {
            Some(newline) => (&field[..newline], Some(&field[newline + 1..])),
            None => (field, None),
        };
        if key.is_empty() {
            self.failed = true;
            return;
        }
        let text = |value: &[u8]| -> Arc<str> { String::from_utf8_lossy(value).into() };
        if key == b"core.fsmonitor" {
            self.fsmonitor = value.is_none_or(is_git_true);
        } else if key == b"remote.pushdefault" {
            if let Some(value) = value {
                self.config.push_default = Some(text(value));
            }
        } else if let Some(name) = key
            .strip_prefix(b"remote.")
            .and_then(|rest| rest.strip_suffix(b".url"))
        {
            let Some(url) = value else {
                return;
            };
            let name = text(name);
            let remotes = &mut self.config.remotes;
            if !name.is_empty()
                && remotes.len() < MAXIMUM_REMOTES
                && !remotes.iter().any(|(existing, _)| *existing == name)
            {
                remotes.push((name, text(url)));
            }
        } else if let Some(variable) = self
            .branch_prefix
            .as_deref()
            .and_then(|prefix| key.strip_prefix(prefix))
        {
            let slot = match variable {
                b"remote" => &mut self.config.branch_remote,
                b"merge" => &mut self.config.branch_merge,
                b"pushremote" => &mut self.config.branch_push_remote,
                _ => return,
            };
            if let Some(value) = value {
                *slot = Some(text(value));
            }
        }
    }
}

/// Git's boolean true spellings. A hook program path, or anything else, is not true.
fn is_git_true(value: &[u8]) -> bool {
    [&b"true"[..], b"yes", b"on", b"1"]
        .iter()
        .any(|truth| value.eq_ignore_ascii_case(truth))
}

/// Parses one complete configuration output held in memory.
pub(crate) fn parse_config(
    output: &[u8],
    branch: Option<&str>,
    git_version: ToolVersion,
) -> Result<RepositoryConfig, RepositoryReadError> {
    let mut parser = ConfigParser::new(branch, git_version);
    parser.push(output);
    parser.finish()
}

/// What `HEAD` names, from the status headers. Branch names come only from `branch.head`.
pub(crate) fn repository_head(
    headers: &StatusHeaders,
) -> Result<RepositoryHead, RepositoryReadError> {
    match (&headers.branch, short_commit(headers)) {
        (Some(branch), Some(_)) => Ok(RepositoryHead::Branch(Arc::clone(branch))),
        (Some(branch), None) => Ok(RepositoryHead::Unborn(Arc::clone(branch))),
        (None, Some(commit)) => Ok(RepositoryHead::Detached(commit)),
        (None, None) => Err(RepositoryReadError::InvalidResponse),
    }
}

/// The abbreviated commit id, or `None` for an unborn branch.
pub(crate) fn short_commit(headers: &StatusHeaders) -> Option<Arc<str>> {
    headers
        .oid
        .as_deref()
        .map(|oid| oid.get(..SHORT_COMMIT_LENGTH).unwrap_or(oid).into())
}

/// The configured upstream. Without `branch.ab`, the upstream no longer exists.
pub(crate) fn upstream(headers: &StatusHeaders) -> Option<Upstream> {
    headers.upstream.as_ref().map(|name| Upstream {
        name: Arc::clone(name),
        divergence: headers.divergence,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository_status::Divergence;

    const HOME: &str = "/Users/person";
    const VERSION: ToolVersion = ToolVersion {
        major: 2,
        minor: 39,
        patch: 5,
    };
    const OID: &str = "1bccb23a55670b76916d70699c1b04ef507bb38d";

    fn discover(output: &str, directory: &str) -> Result<Discovery, RepositoryReadError> {
        parse_discovery(output.as_bytes(), directory, HOME)
    }

    fn repository(toplevel: &str, git: &str, common: &str, prefix: &str) -> Discovery {
        Discovery::Repository(DiscoveredRepository {
            toplevel: toplevel.into(),
            git_directory: git.into(),
            common_directory: common.into(),
            prefix: prefix.into(),
        })
    }

    #[test]
    fn discovery_at_the_top_level_should_join_the_relative_common_directory() {
        let output = "false\nfalse\n/src/app/.git\n.git\n/src/app\n\n";

        assert_eq!(
            discover(output, "/src/app/"),
            Ok(repository("/src/app", "/src/app/.git", "/src/app/.git", ""))
        );
    }

    #[test]
    fn discovery_below_the_top_level_should_keep_the_prefix() {
        let output = "false\nfalse\n/src/app/.git\n../../.git\n/src/app\nsub/deep/\n";

        assert_eq!(
            discover(output, "/src/app/sub/deep"),
            Ok(repository(
                "/src/app",
                "/src/app/.git",
                "/src/app/sub/deep/../../.git",
                "sub/deep/"
            ))
        );
    }

    #[test]
    fn discovery_in_a_linked_worktree_should_keep_the_absolute_common_directory() {
        let output = "false\nfalse\n/src/app/.git/worktrees/wt\n/src/app/.git\n/src/wt\na/\n";

        assert_eq!(
            discover(output, "/src/wt/a"),
            Ok(repository(
                "/src/wt",
                "/src/app/.git/worktrees/wt",
                "/src/app/.git",
                "a/"
            ))
        );
    }

    #[test]
    fn empty_discovery_output_should_mean_no_repository() {
        assert_eq!(discover("", "/"), Ok(Discovery::NotRepository));
    }

    #[test]
    fn git_directories_and_bare_repositories_should_be_hidden() {
        for output in [
            "true\nfalse\n/src/app/.git\n.\n",
            "true\nfalse\n/src/app/.git\n/src/app/.git\n",
            "true\ntrue\n/src/bare.git\n.\n",
            "false\ntrue\n/src/bare.git\n.\n",
            "true\n",
        ] {
            assert_eq!(discover(output, "/src"), Ok(Discovery::Hidden), "{output:?}");
        }
    }

    #[test]
    fn a_home_repository_should_show_only_at_home_itself() {
        let at_home = format!("false\nfalse\n{HOME}/.git\n.git\n{HOME}\n\n");
        let below_home = format!("false\nfalse\n{HOME}/.git\n../.git\n{HOME}\nProjects/\n");

        assert_eq!(
            discover(&at_home, HOME),
            Ok(repository(HOME, "/Users/person/.git", "/Users/person/.git", ""))
        );
        assert_eq!(
            discover(&below_home, "/Users/person/Projects"),
            Ok(Discovery::Hidden)
        );
        assert_eq!(
            parse_discovery(below_home.as_bytes(), "/Users/person/Projects", "/Users/person/"),
            Ok(Discovery::Hidden)
        );
    }

    #[test]
    fn a_repository_below_home_should_not_be_hidden_by_the_home_rule() {
        let output = format!("false\nfalse\n{HOME}/app/.git\n../.git\n{HOME}/app\nsrc/\n");

        assert!(matches!(
            discover(&output, "/Users/person/app/src"),
            Ok(Discovery::Repository(_))
        ));
    }

    #[test]
    fn malformed_discovery_output_should_be_an_invalid_response() {
        for output in [
            &b"maybe\nfalse\n"[..],
            b"false\n",
            b"false\nfalse\n/a/.git\n.git\n/a\n",
            b"false\nfalse\n/a/.git\n.git\n/a\n\n\n",
            b"false\nfalse\n/a/.git\n.git\n/a\nprefix/",
            b"false\nfalse\n\n.git\n/a\n\n",
            b"false\nfalse\n/a/.git\n.git\n\n\n",
            b"false\nfalse\n/a/.git\n.git\n/a\xff\n\n",
            b"false\nmaybe\n/a/.git\n.git\n/a\n\n",
        ] {
            assert_eq!(
                parse_discovery(output, "/a", HOME),
                Err(RepositoryReadError::InvalidResponse),
                "{output:?}"
            );
        }
    }

    #[test]
    fn discovered_repository_debug_should_redact_paths() {
        let Ok(Discovery::Repository(repository)) =
            discover("false\nfalse\n/secret/.git\n.git\n/secret\n\n", "/secret")
        else {
            panic!("expected a repository");
        };

        assert!(!format!("{repository:?}").contains("secret"));
    }

    fn config(output: &str, branch: Option<&str>) -> RepositoryConfig {
        parse_config(output.as_bytes(), branch, VERSION).unwrap()
    }

    #[test]
    fn config_should_keep_remotes_in_order_and_only_the_current_branch() {
        let output = "remote.origin.url\ngit@github.com:me/app.git\0\
             remote.upstream.url\nhttps://github.com/org/app\0\
             remote.my.fork.url\nhttps://example.com/fork\0\
             remote.pushdefault\norigin\0\
             branch.main.remote\nupstream\0\
             branch.main.merge\nrefs/heads/main\0\
             branch.feature.x.remote\norigin\0\
             branch.feature.x.merge\nrefs/heads/feature.x\0\
             branch.feature.x.pushremote\nfork\0";

        assert_eq!(
            config(output, Some("feature.x")),
            RepositoryConfig {
                fsmonitor: FsmonitorPolicy::Disabled,
                remotes: vec![
                    ("origin".into(), "git@github.com:me/app.git".into()),
                    ("upstream".into(), "https://github.com/org/app".into()),
                    ("my.fork".into(), "https://example.com/fork".into()),
                ],
                push_default: Some("origin".into()),
                branch_remote: Some("origin".into()),
                branch_merge: Some("refs/heads/feature.x".into()),
                branch_push_remote: Some("fork".into()),
            }
        );
    }

    #[test]
    fn config_should_ignore_branch_settings_when_head_is_detached() {
        let output = "branch.main.remote\norigin\0branch.main.merge\nrefs/heads/main\0";

        assert_eq!(config(output, None), RepositoryConfig::default());
    }

    #[test]
    fn config_should_not_match_a_branch_that_only_shares_a_prefix() {
        let output = "branch.main.x.remote\norigin\0branch.mai.remote\norigin\0";

        assert_eq!(config(output, Some("main")).branch_remote, None);
    }

    #[test]
    fn config_should_let_later_values_win_except_for_a_remote_url() {
        let output = "remote.origin.url\nfirst\0remote.origin.url\nsecond\0\
             branch.main.remote\norigin\0branch.main.remote\nupstream\0\
             remote.pushdefault\na\0remote.pushdefault\nb\0";

        let config = config(output, Some("main"));

        assert_eq!(config.remotes, [("origin".into(), "first".into())]);
        assert_eq!(config.branch_remote.as_deref(), Some("upstream"));
        assert_eq!(config.push_default.as_deref(), Some("b"));
    }

    #[test]
    fn config_should_cap_distinct_remotes() {
        let output: String = (0..40)
            .map(|index| format!("remote.r{index}.url\nhttps://example.com/{index}\0"))
            .collect();

        let remotes = config(&output, None).remotes;

        assert_eq!(remotes.len(), MAXIMUM_REMOTES);
        assert_eq!(&*remotes[31].0, "r31");
    }

    #[test]
    fn config_should_skip_valueless_strings_and_unknown_keys() {
        let output = "remote.origin.url\0remote..url\nx\0branch.main.remote\0\
             branch.main.description\nx\0user.name\nx\0";

        assert_eq!(config(output, Some("main")), RepositoryConfig::default());
    }

    #[test]
    fn fsmonitor_should_be_builtin_only_for_a_git_true_on_a_new_enough_git() {
        for (record, expected) in [
            ("core.fsmonitor\ntrue\0", FsmonitorPolicy::Builtin),
            ("core.fsmonitor\nTRUE\0", FsmonitorPolicy::Builtin),
            ("core.fsmonitor\nyes\0", FsmonitorPolicy::Builtin),
            ("core.fsmonitor\non\0", FsmonitorPolicy::Builtin),
            ("core.fsmonitor\n1\0", FsmonitorPolicy::Builtin),
            ("core.fsmonitor\0", FsmonitorPolicy::Builtin),
            ("core.fsmonitor\nfalse\0", FsmonitorPolicy::Disabled),
            ("core.fsmonitor\n0\0", FsmonitorPolicy::Disabled),
            ("core.fsmonitor\n\0", FsmonitorPolicy::Disabled),
            (
                "core.fsmonitor\n.git/hooks/fsmonitor-watchman\0",
                FsmonitorPolicy::Disabled,
            ),
            (
                "core.fsmonitor\ntrue\0core.fsmonitor\n/usr/local/bin/hook\0",
                FsmonitorPolicy::Disabled,
            ),
            (
                "core.fsmonitor\n/hook\0core.fsmonitor\ntrue\0",
                FsmonitorPolicy::Builtin,
            ),
            ("", FsmonitorPolicy::Disabled),
        ] {
            assert_eq!(
                config(record, None).fsmonitor,
                expected,
                "{record:?}"
            );
        }
    }

    #[test]
    fn fsmonitor_should_be_disabled_before_git_2_36() {
        let old = ToolVersion {
            major: 2,
            minor: 35,
            patch: 9,
        };

        let config = parse_config(b"core.fsmonitor\ntrue\0", None, old).unwrap();

        assert_eq!(config.fsmonitor, FsmonitorPolicy::Disabled);
        assert_eq!(
            parse_config(b"core.fsmonitor\ntrue\0", None, BUILTIN_FSMONITOR_GIT_VERSION)
                .unwrap()
                .fsmonitor,
            FsmonitorPolicy::Builtin
        );
    }

    #[test]
    fn config_should_parse_identically_at_every_chunk_boundary() {
        let output = b"remote.origin.url\nhttps://github.com/a/b\0core.fsmonitor\ntrue\0\
             branch.main.merge\nrefs/heads/main\0";
        let expected = parse_config(output, Some("main"), VERSION).unwrap();

        for split in 0..=output.len() {
            let mut parser = ConfigParser::new(Some("main"), VERSION);
            parser.push(&output[..split]);
            parser.push(&output[split..]);
            assert_eq!(parser.finish().unwrap(), expected, "split at {split}");
        }
    }

    #[test]
    fn oversized_config_records_should_be_skipped() {
        let mut parser = ConfigParser::new(Some("main"), VERSION);
        parser.push(b"remote.huge.url\n");
        for _ in 0..8 {
            parser.push(&[b'a'; 16 * 1024]);
        }
        parser.push(b"\0remote.origin.url\nhttps://github.com/a/b\0");

        assert_eq!(
            parser.finish().unwrap().remotes,
            [("origin".into(), "https://github.com/a/b".into())]
        );
    }

    #[test]
    fn malformed_config_output_should_be_an_invalid_response() {
        for output in [&b"remote.origin.url\nx"[..], b"\0", b"\nvalue\0"] {
            assert_eq!(
                parse_config(output, None, VERSION),
                Err(RepositoryReadError::InvalidResponse),
                "{output:?}"
            );
        }
    }

    fn headers(oid: Option<&str>, branch: Option<&str>) -> StatusHeaders {
        StatusHeaders {
            oid: oid.map(Into::into),
            branch: branch.map(Into::into),
            ..StatusHeaders::default()
        }
    }

    #[test]
    fn head_should_come_from_the_branch_header() {
        let branch = headers(Some(OID), Some("feature/a"));
        let detached = headers(Some(OID), None);
        let unborn = headers(None, Some("main"));

        assert_eq!(
            repository_head(&branch),
            Ok(RepositoryHead::Branch("feature/a".into()))
        );
        assert_eq!(
            repository_head(&detached),
            Ok(RepositoryHead::Detached("1bccb23".into()))
        );
        assert_eq!(
            repository_head(&unborn),
            Ok(RepositoryHead::Unborn("main".into()))
        );
        assert_eq!(short_commit(&branch).as_deref(), Some("1bccb23"));
        assert_eq!(short_commit(&unborn), None);
        assert_eq!(
            repository_head(&headers(None, None)),
            Err(RepositoryReadError::InvalidResponse)
        );
    }

    #[test]
    fn upstream_should_carry_divergence_or_be_gone_without_it() {
        let divergence = Divergence {
            ahead: 1,
            behind: 2,
        };
        let tracking = StatusHeaders {
            upstream: Some("origin/main".into()),
            divergence: Some(divergence),
            ..headers(Some(OID), Some("main"))
        };
        let gone = StatusHeaders {
            upstream: Some("origin/deleted".into()),
            ..headers(Some(OID), Some("main"))
        };

        assert_eq!(
            upstream(&tracking),
            Some(Upstream {
                name: "origin/main".into(),
                divergence: Some(divergence),
            })
        );
        assert_eq!(
            upstream(&gone),
            Some(Upstream {
                name: "origin/deleted".into(),
                divergence: None,
            })
        );
        assert_eq!(upstream(&headers(Some(OID), Some("main"))), None);
    }
}
