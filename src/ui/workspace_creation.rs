//! Canonical Workspace creation rows shared by the sidebar footer menu and the Workspace Switcher.
//!
//! Remote rows name one saved paired server each. Adding a server belongs to Settings > Servers >
//! Add Server, so no creation row enters a host or installs anything.

use gpui::{Action, SharedString};
use spaceterm_ui::CustomIconName;

use crate::desktop_profile::DesktopPresentation;
use crate::direct_connections::DirectRouteId;

/// One way to create a Workspace from the sidebar footer menu or the Workspace Switcher.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkspaceCreation {
    /// A Workspace the account server owns, at the local home directory.
    Local,
    /// A Workspace the account server owns, pinned to a local directory chosen before creation.
    OpenLocalDirectory,
    /// A Workspace on a saved paired server, at that server's home directory.
    Remote(DirectRouteId),
    /// A Workspace on a saved paired server, pinned to a directory chosen on that server.
    OpenRemoteDirectory(DirectRouteId),
}

/// A saved paired server the remote creation rows offer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RemoteCreationServer {
    pub(crate) route: DirectRouteId,
    pub(crate) name: SharedString,
    /// Whether this window presents the server now, so it can take a new Workspace.
    pub(crate) available: bool,
}

/// Why a saved server's creation rows are disabled.
pub(crate) const REMOTE_UNAVAILABLE: &str = "Not connected";

impl WorkspaceCreation {
    /// The local creation rows in presentation order. Remote rows follow per saved server.
    pub(crate) const LOCAL: [Self; 2] = [Self::Local, Self::OpenLocalDirectory];

    /// The row label as the Workspace Switcher shows it, naming the server for a remote row.
    /// Open rows end in an ellipsis because a chooser opens before creation.
    pub(crate) fn label(self, server: Option<&str>) -> SharedString {
        match (self, server) {
            (Self::Local, _) => "Local Workspace".into(),
            (Self::OpenLocalDirectory, _) => "Open Local Directory\u{2026}".into(),
            (Self::Remote(_), Some(server)) => format!("New Workspace on {server}").into(),
            (Self::OpenRemoteDirectory(_), Some(server)) => {
                format!("Open Directory on {server}\u{2026}").into()
            }
            (Self::Remote(_), None) => "Remote Workspace".into(),
            (Self::OpenRemoteDirectory(_), None) => "Open Remote Directory\u{2026}".into(),
        }
    }

    pub(crate) const fn icon(self) -> CustomIconName {
        match self {
            Self::Local => CustomIconName::RectangleStackBadgePlus,
            Self::Remote(_) => CustomIconName::GlobePlus,
            Self::OpenLocalDirectory => CustomIconName::FolderBadgePlus,
            Self::OpenRemoteDirectory(_) => CustomIconName::FolderBadgeGlobe,
        }
    }

    /// The row's stable selector suffix, shared by both surfaces. Remote rows carry the server's
    /// position among the saved servers.
    pub(crate) fn selector(self, position: usize) -> String {
        match self {
            Self::Local => "create-local".to_owned(),
            Self::OpenLocalDirectory => "open-local-directory".to_owned(),
            Self::Remote(_) => format!("create-remote-{position}"),
            Self::OpenRemoteDirectory(_) => format!("open-remote-directory-{position}"),
        }
    }

    /// The row's Shortcut. Only the local rows are Commands.
    pub(crate) fn shortcut(self, presentation: &DesktopPresentation) -> Option<SharedString> {
        presentation.shortcut(self.action()?.as_ref())
    }

    fn action(self) -> Option<Box<dyn Action>> {
        match self {
            Self::Local => Some(Box::new(super::NewWorkspace)),
            Self::OpenLocalDirectory => Some(Box::new(super::OpenLocalDirectory)),
            Self::Remote(_) | Self::OpenRemoteDirectory(_) => None,
        }
    }
}
