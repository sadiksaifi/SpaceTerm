//! The three Workspace creation Commands, as the sidebar's Create Workspace menu, the Workspace
//! Switcher and the File menu present them.
//!
//! New Scratch Workspace opens an unpinned Local Workspace at once. New Workspace and New Remote
//! Workspace open a chooser first, so their rows end in an ellipsis: the Directory Picker on This
//! Mac or This Computer, or the Machine chooser and then the Directory Picker on that Machine.

use gpui::{Action, SharedString};
use spaceterm_ui::CustomIconName;

use crate::desktop_profile::DesktopPresentation;
use crate::keybindings::Command;

/// One way to create a Workspace.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkspaceCreation {
    /// An unpinned Local Workspace at the local home directory.
    Scratch,
    /// A Local Workspace pinned to a directory chosen before creation.
    Local,
    /// A Remote Workspace on a chosen paired Machine, pinned to a directory chosen there.
    Remote,
}

impl WorkspaceCreation {
    /// Every creation Command in presentation order.
    pub(crate) const ALL: [Self; 3] = [Self::Scratch, Self::Local, Self::Remote];

    pub(crate) const fn command(self) -> Command {
        match self {
            Self::Scratch => Command::NewScratchWorkspace,
            Self::Local => Command::NewWorkspace,
            Self::Remote => Command::NewRemoteWorkspace,
        }
    }

    /// The row label: the Command's label, with an ellipsis when a chooser opens first.
    pub(crate) fn label(self) -> SharedString {
        let label = self.command().label();
        match self {
            Self::Scratch => label.into(),
            Self::Local | Self::Remote => format!("{label}\u{2026}").into(),
        }
    }

    pub(crate) const fn icon(self) -> CustomIconName {
        match self {
            Self::Scratch => CustomIconName::RectangleStackBadgePlus,
            Self::Local => CustomIconName::FolderBadgePlus,
            Self::Remote => CustomIconName::FolderBadgeGlobe,
        }
    }

    /// The row's stable selector suffix, shared by every surface.
    pub(crate) const fn selector(self) -> &'static str {
        match self {
            Self::Scratch => "new-scratch-workspace",
            Self::Local => "new-workspace",
            Self::Remote => "new-remote-workspace",
        }
    }

    /// The row's Shortcut.
    pub(crate) fn shortcut(self, presentation: &DesktopPresentation) -> Option<SharedString> {
        presentation.shortcut(self.action().as_ref())
    }

    fn action(self) -> Box<dyn Action> {
        self.command().action()
    }
}
