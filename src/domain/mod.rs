pub(crate) mod remote_project;
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
    CreateRemoteProjectOutcome, DirectoryAuthority, DirectoryChange, RemoteDirectoryIdentity,
    RemoteWorkspaceDirectory, RemoteWorkspaceKey, RemoteWorkspaceValueError, SshDestination,
    ValidatedWorkspaceDirectory, WorkspaceCollection, WorkspaceDirectoryAvailability,
    WorkspaceDirectoryIdentity, WorkspaceError, WorkspaceId, WorkspaceKind,
};

pub(crate) use remote_project::{
    RemoteConnectionPhase, RemoteConnectionReduction, RemoteConnectionState,
};
