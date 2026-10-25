//! Client Git presentation preferences.

use crate::repository_status::RepositoryStatusPreferences;

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct GitPreferences {
    pub(crate) show_repository_status: bool,
    /// Has no effect while Repository Status is hidden.
    pub(crate) show_pull_requests: bool,
}

impl Default for GitPreferences {
    fn default() -> Self {
        let status = RepositoryStatusPreferences::default();
        Self {
            show_repository_status: status.show_repository_status,
            show_pull_requests: status.show_pull_requests,
        }
    }
}

impl GitPreferences {
    pub(crate) const fn repository_status(&self) -> RepositoryStatusPreferences {
        RepositoryStatusPreferences {
            show_repository_status: self.show_repository_status,
            show_pull_requests: self.show_pull_requests,
        }
    }
}
