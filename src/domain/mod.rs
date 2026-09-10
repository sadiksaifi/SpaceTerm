pub(crate) mod remote_workspace;
mod tab_collection;
mod terminal_tab;
mod workspace_collection;

pub(crate) use crate::close_confirmation::{
    ClosePaneOutcome, CloseTabOutcome, CloseWorkspaceOutcome, FinalTabCloseOutcome,
};
pub(crate) use tab_collection::{TabCollection, TabError};
pub(crate) use terminal_tab::{
    FocusDirection, PaneId, PaneNodeRef, PaneSize, PaneSizeError, PaneTreeRef, SplitAxis, SplitId,
    TabId, TerminalTab, ZoomState,
};
pub(crate) use workspace_collection::{
    CurrentDirectory, DirectoryAvailability, LocalDirectoryIdentity, PinnedDirectory,
    RemoteDirectory, RemoteDirectoryIdentity, RemoteUser, RemoteWorkspaceTarget,
    RemoteWorkspaceValueError, SshDestination, ValidatedLocalDirectory, WorkspaceCollection,
    WorkspaceError, WorkspaceId, WorkspaceLocation,
};

pub(crate) use remote_workspace::{
    RemoteConnectionPhase, RemoteConnectionReduction, RemoteConnectionState,
};
