//! The Sidebar Settings: how the Workspace sidebar starts in a new Operating-System Window.

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct SidebarPreferences {
    /// Toggle Sidebar still shows or hides the sidebar after the window opens.
    pub(crate) default_state: SidebarState,
}

#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, PartialEq, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SidebarState {
    #[default]
    Expanded,
    Collapsed,
}

impl SidebarState {
    pub(crate) const fn is_expanded(self) -> bool {
        matches!(self, Self::Expanded)
    }
}
