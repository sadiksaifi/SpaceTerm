mod tab_collection;
mod terminal_tab;
mod workspace_collection;

pub(crate) use tab_collection::{CloseTabOutcome, TabCollection, TabError};
pub(crate) use terminal_tab::{
    ClosePaneOutcome, FocusDirection, PaneId, PaneNodeRef, PaneSize, PaneSizeError, PaneTreeRef,
    SplitAxis, SplitId, TabId, TerminalTab, ZoomState,
};
pub(crate) use workspace_collection::{
    CloseWorkspaceOutcome, CreateRemoteProjectOutcome, DirectoryAuthority, FinalTabCloseOutcome,
    RemoteConnectionPhase, RemoteConnectionReduction, RemoteConnectionState,
    RemoteDirectoryIdentity, RemoteWorkspaceDirectory, RemoteWorkspaceKey,
    RemoteWorkspaceValueError, SshDestination, ValidatedWorkspaceDirectory, WorkspaceCollection,
    WorkspaceDirectoryAvailability, WorkspaceDirectoryIdentity, WorkspaceError, WorkspaceId,
    WorkspaceKind,
};
