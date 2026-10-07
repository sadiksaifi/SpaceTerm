//! Repository Status adapters every Unix host composes the same way.

use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;

use crate::repository_status::RepositoryToolDiscovery;
use crate::ui::repository_status_store::RepositoryStatusAdapters;

/// Launch environment entries the GitHub CLI may need to reach and trust its host. SpaceTerm
/// passes them through in memory and stores none of them.
const GITHUB_CLI_PASSTHROUGH: [&str; 18] = [
    "GH_CONFIG_DIR",
    "GH_HOST",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "XDG_CONFIG_HOME",
    "XDG_STATE_HOME",
    "XDG_DATA_HOME",
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "ALL_PROXY",
    "NO_PROXY",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
];

pub(crate) fn repository_status_adapters(
    home: &Path,
    discovery: Arc<dyn RepositoryToolDiscovery>,
) -> RepositoryStatusAdapters {
    RepositoryStatusAdapters {
        runner: Arc::new(super::unix_repository_program::UnixRepositoryProgramRunner::new()),
        discovery,
        markers: Arc::new(super::repository_marker_reader::UnixRepositoryMarkerReader::new()),
        watcher: Arc::new(super::repository_watch::NotifyRepositoryWatcher::new()),
        // git prints resolved paths, so the home rule compares against the resolved home.
        physical_home: std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf()),
        github_cli_environment: github_cli_environment(|key| std::env::var_os(key)),
    }
}

fn github_cli_environment(
    variable: impl Fn(&str) -> Option<OsString>,
) -> Vec<(OsString, OsString)> {
    GITHUB_CLI_PASSTHROUGH
        .iter()
        .filter_map(|key| variable(key).map(|value| (OsString::from(key), value)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_listed_variables_reach_the_github_cli() {
        let environment = github_cli_environment(|key| match key {
            "HTTPS_PROXY" => Some("http://proxy:3128".into()),
            "GH_HOST" => Some("github.example.com".into()),
            _ => None,
        });
        assert_eq!(
            environment,
            [
                ("GH_HOST".into(), "github.example.com".into()),
                ("HTTPS_PROXY".into(), "http://proxy:3128".into()),
            ]
        );
    }
}
