//! Canonical Workspace creation rows shared by the sidebar footer menu and the Workspace Switcher.

use gpui::{Action, SharedString};
use spaceterm_ui::CustomIconName;

use crate::desktop_profile::DesktopPresentation;

/// One way to create a Workspace from the sidebar footer menu or the Workspace Switcher.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkspaceCreation {
    /// A Local Workspace at the local home directory.
    Local,
    /// A Remote Workspace at the remote home directory.
    Remote,
    /// A Local Workspace pinned to a directory chosen before creation.
    OpenLocalDirectory,
    /// A Remote Workspace pinned to a directory chosen after connecting.
    OpenRemoteDirectory,
    /// A Workspace the adopted SpaceTerm Server owns and every connected client presents. The row
    /// appears only while a Workspace Window presents that server's Workspaces.
    Server,
}

impl WorkspaceCreation {
    /// Every client-owned creation row in presentation order.
    pub(crate) const ALL: [Self; 4] = [
        Self::Local,
        Self::Remote,
        Self::OpenLocalDirectory,
        Self::OpenRemoteDirectory,
    ];

    /// The creation rows one surface offers. The server row joins them only while a Workspace
    /// Window presents an adopted server's Workspaces.
    pub(crate) fn rows(server: bool) -> Vec<Self> {
        let mut rows = Self::ALL.to_vec();
        if server {
            rows.push(Self::Server);
        }
        rows
    }

    /// The row label. Open rows end in an ellipsis because a chooser opens before creation.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Local => "Local Workspace",
            Self::Remote => "Remote Workspace",
            Self::OpenLocalDirectory => "Open Local Directory…",
            Self::OpenRemoteDirectory => "Open Remote Directory…",
            Self::Server => "Server Workspace",
        }
    }

    pub(crate) const fn icon(self) -> CustomIconName {
        match self {
            Self::Local => CustomIconName::RectangleStackBadgePlus,
            Self::Remote => CustomIconName::GlobePlus,
            Self::OpenLocalDirectory => CustomIconName::FolderBadgePlus,
            Self::OpenRemoteDirectory => CustomIconName::FolderBadgeGlobe,
            Self::Server => CustomIconName::RectangleStackBadgePlus,
        }
    }

    pub(crate) const fn is_remote(self) -> bool {
        matches!(self, Self::Remote | Self::OpenRemoteDirectory)
    }

    /// Whether the row begins a group, separated from the rows above it.
    pub(crate) const fn starts_group(self) -> bool {
        matches!(self, Self::OpenLocalDirectory | Self::Server)
    }

    /// The row's stable selector suffix, shared by both surfaces.
    pub(crate) const fn selector(self) -> &'static str {
        match self {
            Self::Local => "create-local",
            Self::Remote => "create-remote",
            Self::OpenLocalDirectory => "open-local-directory",
            Self::OpenRemoteDirectory => "open-remote-directory",
            Self::Server => "create-server",
        }
    }

    /// The row's Shortcut. The server row is no Command, so it has none.
    pub(crate) fn shortcut(self, presentation: &DesktopPresentation) -> Option<SharedString> {
        presentation.shortcut(self.action()?.as_ref())
    }

    fn action(self) -> Option<Box<dyn Action>> {
        match self {
            Self::Local => Some(Box::new(super::NewWorkspace)),
            Self::Remote => Some(Box::new(super::NewRemoteWorkspace)),
            Self::OpenLocalDirectory => Some(Box::new(super::OpenLocalDirectory)),
            Self::OpenRemoteDirectory => Some(Box::new(super::OpenRemoteDirectory)),
            Self::Server => None,
        }
    }
}

/// Trigger tooltip for the sidebar creation menu. The trigger stays enabled while Remote creation
/// is unavailable, so the tooltip appends the reason.
pub(crate) fn new_workspace_trigger_tooltip(remote_unavailable: Option<&str>) -> SharedString {
    match remote_unavailable {
        Some(reason) => format!("New Workspace: {reason}").into(),
        None => "New Workspace".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_tooltip_should_surface_remote_unavailable_reason() {
        assert_eq!(
            new_workspace_trigger_tooltip(None).to_string(),
            "New Workspace"
        );
        let reason = "OpenSSH 8.2 or later is required";
        let tooltip = new_workspace_trigger_tooltip(Some(reason)).to_string();
        assert!(
            tooltip.contains("New Workspace"),
            "tooltip should keep trigger context, got {tooltip:?}"
        );
        assert!(
            tooltip.contains(reason),
            "tooltip should surface the reason, got {tooltip:?}"
        );
    }
}
