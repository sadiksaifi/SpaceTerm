//! App-scoped Repository Status state shared by every window.

use gpui::Global;

use crate::repository_status::{GitHubCliStatus, GitToolStatus};

/// What SpaceTerm last learned about the programs Repository Status runs.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct RepositoryTools {
    pub(crate) git: GitToolStatus,
    pub(crate) github_cli: GitHubCliStatus,
}

impl Global for RepositoryTools {}
