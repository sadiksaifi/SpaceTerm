/// The Settings that decide which repository facts SpaceTerm presents.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct RepositoryStatusPreferences {
    pub(crate) show_repository_status: bool,
    /// Has no effect while Repository Status is hidden.
    pub(crate) show_pull_requests: bool,
}

impl Default for RepositoryStatusPreferences {
    fn default() -> Self {
        Self {
            show_repository_status: true,
            show_pull_requests: true,
        }
    }
}

impl RepositoryStatusPreferences {
    /// Whether SpaceTerm looks up Pull Requests at all.
    pub(crate) const fn pull_requests_enabled(self) -> bool {
        self.show_repository_status && self.show_pull_requests
    }
}
