mod terminal_window;
mod window_collection;
mod workspace_collection;

pub(crate) use terminal_window::{
    ClosePaneOutcome, FocusDirection, PaneId, PaneNodeRef, PaneSize, PaneSizeError, PaneTreeRef,
    SplitAxis, SplitId, TerminalWindow, WindowId, ZoomState,
};
pub(crate) use window_collection::{CloseWindowOutcome, WindowCollection, WindowError};
pub(crate) use workspace_collection::{
    CloseWorkspaceOutcome, CreateRemoteProjectOutcome, DirectoryAuthority, FinalWindowCloseOutcome,
    RemoteConnectionPhase, RemoteConnectionReduction, RemoteConnectionState,
    RemoteDirectoryIdentity, RemoteWorkspaceDirectory, RemoteWorkspaceKey,
    RemoteWorkspaceValueError, SshDestination, ValidatedWorkspaceDirectory, WorkspaceCollection,
    WorkspaceDirectoryAvailability, WorkspaceDirectoryIdentity, WorkspaceError, WorkspaceId,
    WorkspaceKind,
};
