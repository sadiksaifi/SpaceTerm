pub(crate) mod remote_workspace;
mod tab;
mod tab_collection;
mod workspace_collection;

pub(crate) use crate::close_confirmation::{
    ClosePaneOutcome, CloseTabOutcome, CloseWorkspaceOutcome, FinalTabCloseOutcome,
};
pub(crate) use tab::{
    FocusDirection, PaneEdge, PaneId, PaneNodeRef, PaneSize, PaneSizeError, PaneTreeRef, SplitAxis,
    SplitId, Tab, TabId, ZoomState,
};
pub(crate) use tab_collection::{TabCollection, TabError, TabStep};
#[cfg(test)]
pub(crate) use workspace_collection::tests::set_next_workspace_id;
pub(crate) use workspace_collection::{
    CurrentDirectory, DirectoryAvailability, LocalDirectoryIdentity, PinnedDirectory,
    RemoteDirectory, RemoteDirectoryIdentity, RemoteUser, RemoteWorkspaceTarget, SshDestination,
    ValidatedLocalDirectory, WorkspaceCollection, WorkspaceEntry, WorkspaceError, WorkspaceId,
    WorkspaceLocation,
};

pub(crate) use remote_workspace::{
    RemoteConnectionPhase, RemoteConnectionReduction, RemoteConnectionState,
};
