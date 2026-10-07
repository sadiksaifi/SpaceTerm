//! Turns the raw remote probe and count fields into the facts local reads produce, with the same
//! parsers.

use std::sync::Arc;

use super::discovery::{
    Discovery, parse_config, parse_discovery, repository_head, short_commit, upstream,
};
use super::operation::repository_operation;
use super::porcelain::{PorcelainParser, parse_porcelain};
use super::tools::{git_tool_status, parse_git_version};
use super::{
    ChangeSummary, GitToolStatus, ProbeOutcome, ProbedRepository, RemoteProbeOutcome,
    RemoteRepositoryCount, RemoteRepositoryProbe, RepositoryReadError, RepositoryRoot,
};

/// The probe outcome for `directory`, the remote spelling the probe ran in.
pub(crate) fn remote_probe_outcome(
    probe: RemoteRepositoryProbe,
    directory: &str,
) -> Result<ProbeOutcome, RepositoryReadError> {
    match probe.outcome {
        RemoteProbeOutcome::Repository => {}
        RemoteProbeOutcome::NotRepository | RemoteProbeOutcome::DirectoryUnavailable => {
            return Ok(ProbeOutcome::NotRepository);
        }
        RemoteProbeOutcome::GitMissing => return Err(RepositoryReadError::ToolMissing),
    }
    let version = parse_git_version(&probe.git_version).ok_or(RepositoryReadError::ToolMissing)?;
    let GitToolStatus::Ready(version) = git_tool_status(version) else {
        return Err(RepositoryReadError::ToolMissing);
    };
    let home = std::str::from_utf8(&probe.physical_home)
        .map_err(|_| RepositoryReadError::InvalidResponse)?
        .trim_end_matches('\n');
    let repository = match parse_discovery(&probe.discovery, directory, home)? {
        Discovery::NotRepository => return Ok(ProbeOutcome::NotRepository),
        Discovery::Hidden => return Ok(ProbeOutcome::Hidden),
        Discovery::Repository(repository) => repository,
    };
    if !probe.discovery_succeeded {
        return Err(RepositoryReadError::Unavailable);
    }
    let headers = parse_porcelain(&probe.status_headers)?.headers;
    let head = repository_head(&headers)?;
    let config = parse_config(&probe.config, headers.branch.as_deref(), version)?;
    Ok(ProbeOutcome::Repository(Box::new(ProbedRepository {
        root: RepositoryRoot::Remote(Arc::clone(&repository.toplevel)),
        git_directory: RepositoryRoot::Remote(Arc::clone(&repository.git_directory)),
        common_directory: RepositoryRoot::Remote(Arc::clone(&repository.common_directory)),
        commit: short_commit(&headers),
        upstream: upstream(&headers),
        head,
        operation: repository_operation(&probe.markers),
        config,
    })))
}

/// The change summary of a remote count. A count cut at the remote limit drops its partial last
/// record and reports a lower bound.
pub(crate) fn remote_change_summary(
    count: &RemoteRepositoryCount,
) -> Result<ChangeSummary, RepositoryReadError> {
    let mut parser = PorcelainParser::new();
    parser.push(&count.status);
    let summary = if count.truncated {
        parser.finish_truncated()?
    } else {
        parser.finish()?
    };
    Ok(summary.changes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repository_status::{
        ChangeTotal, FsmonitorPolicy, OperationMarkers, RepositoryHead, RepositoryOperation,
    };

    const HEADERS: &[u8] = b"# branch.oid 0123456789abcdef0123456789abcdef01234567\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +1 -2\0";

    fn probe(discovery: &[u8]) -> RemoteRepositoryProbe {
        RemoteRepositoryProbe {
            outcome: RemoteProbeOutcome::Repository,
            git_version: b"git version 2.45.1\n".to_vec(),
            discovery_succeeded: true,
            discovery: discovery.to_vec(),
            physical_home: b"/home/dev\n".to_vec(),
            status_headers: HEADERS.to_vec(),
            config: b"core.fsmonitor\ntrue\0remote.origin.url\nhttps://github.com/acme/tool.git\0"
                .to_vec(),
            markers: OperationMarkers {
                merge_head: true,
                ..OperationMarkers::default()
            },
        }
    }

    const PROJECT: &[u8] = b"false\nfalse\n/home/dev/tool/.git\n.git\n/home/dev/tool\nsrc/\n";

    #[test]
    fn a_remote_repository_reads_like_a_local_one() {
        let ProbeOutcome::Repository(repository) =
            remote_probe_outcome(probe(PROJECT), "/home/dev/tool/src").unwrap()
        else {
            panic!("expected a repository");
        };
        assert_eq!(
            repository.root,
            RepositoryRoot::Remote("/home/dev/tool".into())
        );
        assert_eq!(
            repository.git_directory,
            RepositoryRoot::Remote("/home/dev/tool/.git".into())
        );
        assert_eq!(repository.head, RepositoryHead::Branch("main".into()));
        assert_eq!(repository.commit.as_deref(), Some("0123456"));
        assert_eq!(repository.operation, Some(RepositoryOperation::Merging));
        assert_eq!(repository.config.fsmonitor, FsmonitorPolicy::Builtin);
        assert_eq!(repository.config.remotes.len(), 1);
        let upstream = repository.upstream.expect("upstream");
        assert_eq!(&*upstream.name, "origin/main");
    }

    #[test]
    fn remote_outcomes_without_a_repository_show_nothing_or_fail_quietly() {
        for (outcome, expected) in [
            (
                RemoteProbeOutcome::NotRepository,
                Ok(ProbeOutcome::NotRepository),
            ),
            (
                RemoteProbeOutcome::DirectoryUnavailable,
                Ok(ProbeOutcome::NotRepository),
            ),
            (
                RemoteProbeOutcome::GitMissing,
                Err(RepositoryReadError::ToolMissing),
            ),
        ] {
            let mut probe = probe(PROJECT);
            probe.outcome = outcome;
            assert_eq!(remote_probe_outcome(probe, "/home/dev/tool"), expected);
        }
    }

    #[test]
    fn a_remote_git_older_than_the_minimum_counts_as_missing() {
        let mut probe = probe(PROJECT);
        probe.git_version = b"git version 2.14.1\n".to_vec();
        assert_eq!(
            remote_probe_outcome(probe, "/home/dev/tool"),
            Err(RepositoryReadError::ToolMissing)
        );
    }

    #[test]
    fn a_git_directory_or_bare_repository_is_hidden_although_discovery_failed() {
        let mut probe = probe(b"true\nfalse\n");
        probe.discovery_succeeded = false;
        assert_eq!(
            remote_probe_outcome(probe, "/home/dev/tool/.git"),
            Ok(ProbeOutcome::Hidden)
        );
    }

    #[test]
    fn a_home_repository_is_hidden_below_home() {
        let probe = probe(b"false\nfalse\n/home/dev/.git\n.git\n/home/dev\nnotes/\n");
        assert_eq!(
            remote_probe_outcome(probe, "/home/dev/notes"),
            Ok(ProbeOutcome::Hidden)
        );
    }

    #[test]
    fn a_failed_discovery_with_repository_lines_is_unavailable() {
        let mut probe = probe(PROJECT);
        probe.discovery_succeeded = false;
        assert_eq!(
            remote_probe_outcome(probe, "/home/dev/tool/src"),
            Err(RepositoryReadError::Unavailable)
        );
    }

    #[test]
    fn a_truncated_count_reports_a_lower_bound() {
        let mut status = HEADERS.to_vec();
        status.extend_from_slice(b"? one.txt\0? two.txt\0? thr");
        let summary = remote_change_summary(&RemoteRepositoryCount {
            status,
            truncated: true,
        })
        .unwrap();
        assert_eq!(summary.total, ChangeTotal::AtLeast(2));
        assert_eq!(summary.untracked, 2);
    }

    #[test]
    fn a_complete_count_is_exact() {
        let mut status = HEADERS.to_vec();
        status.extend_from_slice(b"? one.txt\0");
        let summary = remote_change_summary(&RemoteRepositoryCount {
            status,
            truncated: false,
        })
        .unwrap();
        assert_eq!(summary.total, ChangeTotal::Exact(1));
    }
}
