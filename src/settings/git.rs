//! The Git Settings: Repository Status toggles and the Worktree Path Template.

use crate::repository_status::RepositoryStatusPreferences;
use crate::worktrees::path_template::{
    DEFAULT_WORKTREE_PATH_TEMPLATE, WorktreePathTemplateError, validate,
};

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct GitPreferences {
    pub(crate) show_repository_status: bool,
    /// Has no effect while Repository Status is hidden.
    pub(crate) show_pull_requests: bool,
    /// Where SpaceTerm proposes a new Worktree's directory.
    pub(crate) worktree_path_template: String,
}

impl Default for GitPreferences {
    fn default() -> Self {
        let status = RepositoryStatusPreferences::default();
        Self {
            show_repository_status: status.show_repository_status,
            show_pull_requests: status.show_pull_requests,
            worktree_path_template: DEFAULT_WORKTREE_PATH_TEMPLATE.to_owned(),
        }
    }
}

impl GitPreferences {
    pub(crate) fn validate(&self) -> Result<(), WorktreePathTemplateError> {
        validate(&self.worktree_path_template)
    }

    pub(crate) const fn repository_status(&self) -> RepositoryStatusPreferences {
        RepositoryStatusPreferences {
            show_repository_status: self.show_repository_status,
            show_pull_requests: self.show_pull_requests,
        }
    }
}
