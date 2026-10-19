pub(crate) mod remote_workspace;
pub(crate) mod tab;
pub(crate) mod tab_collection;
pub(crate) mod workspace_collection;
pub(crate) mod worktree;

pub(crate) use crate::close_confirmation::{ClosePaneOutcome, CloseTabOutcome};
pub(crate) use tab::{
    FocusDirection, PaneEdge, PaneId, PaneNodeRef, PaneSize, PaneSizeError, PaneTreeRef, SplitAxis,
    SplitId, Tab, TabId, ZoomState,
};
pub(crate) use tab_collection::{TabCollection, TabError, TabStep};
#[cfg(test)]
pub(crate) use workspace_collection::tests::PresentedWorkspace;
pub(crate) use workspace_collection::{
    CurrentDirectory, LocalDirectoryIdentity, PinnedDirectory, RemoteDirectory,
    RemoteDirectoryIdentity, RemoteUser, RemoteWorkspaceTarget, SshDestination,
    ValidatedLocalDirectory, WorkspaceCollection, WorkspaceEntry, WorkspaceError, WorkspaceId,
    WorkspaceLocation,
};
pub(crate) use worktree::{RepositoryIdentity, WorktreeId, WorktreeKey, WorktreeRegistry};

pub(crate) use remote_workspace::{
    RemoteConnectionPhase, RemoteConnectionReduction, RemoteConnectionState,
};
