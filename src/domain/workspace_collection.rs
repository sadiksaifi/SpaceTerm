mod naming;

use super::remote_workspace::{RemoteConnectionReduction, RemoteConnectionState};
use crate::close_confirmation::{
    CloseContinuation, CloseWorkspaceOutcome, FinalTabCloseOutcome, HierarchyClose,
};

use std::fmt;
use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct WorkspaceId(u64);

impl WorkspaceId {
    const fn from_raw(value: u64) -> Self {
        Self(value)
    }

    #[cfg(test)]
    pub(crate) const fn new(value: u64) -> Self {
        Self::from_raw(value)
    }

    pub(crate) const fn get(self) -> u64 {
        self.0
    }
}

pub(crate) use crate::platform::local_filesystem::LocalDirectoryIdentity;

/// A Terminal Session's directory, retaining its machine boundary.
#[derive(Clone, Eq, PartialEq)]
pub(crate) enum CurrentDirectory {
    Local(std::path::PathBuf),
    Remote(RemoteDirectory),
}

impl std::fmt::Debug for CurrentDirectory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Local(_) => "CurrentDirectory::Local",
            Self::Remote(_) => "CurrentDirectory::Remote",
        })
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
/// Validation failures for values that cross the local-to-remote domain boundary.
///
/// Rejected strings must not reach OpenSSH arguments, remote utility commands, or local path APIs.
pub(crate) enum RemoteWorkspaceValueError {
    #[error("SSH destination must be one non-option, control-free token")]
    Destination,
    #[error("Remote Directory must be an absolute or ~/ path without control characters")]
    StartingDirectory,
    #[error("Physical remote directory identity must be an absolute control-free path")]
    DirectoryIdentity,
}

/// The exact OpenSSH destination token selected by the user.
///
/// Equality is spelling-sensitive so different configured aliases remain distinct even when they
/// currently resolve to the same host. The token is passed as one validated OpenSSH argument.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct SshDestination(String);

impl fmt::Debug for SshDestination {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SshDestination(<redacted>)")
    }
}

impl SshDestination {
    pub(crate) fn new(value: String) -> Result<Self, RemoteWorkspaceValueError> {
        if value.is_empty()
            || value.starts_with('-')
            || value
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(RemoteWorkspaceValueError::Destination);
        }
        Ok(Self(value))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// The exact user-visible spelling of a directory selected on a remote destination.
///
/// This remote value is preserved for display and shell startup. It is not local filesystem
/// authority and must never be converted to `PathBuf` or passed to a local path API.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct RemoteDirectory(String);

impl fmt::Debug for RemoteDirectory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RemoteDirectory(<redacted>)")
    }
}

impl RemoteDirectory {
    pub(crate) fn new(value: String) -> Result<Self, RemoteWorkspaceValueError> {
        let supported_form = value.starts_with('/') || value == "~" || value.starts_with("~/");
        if !supported_form || value.chars().any(char::is_control) {
            return Err(RemoteWorkspaceValueError::StartingDirectory);
        }
        Ok(Self(value))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// The physical absolute directory returned by the remote `pwd -P` validation protocol.
///
/// Its normalized path-like spelling is an opaque remote identity used for connection and pin
/// revalidation. It must never become a local `PathBuf` or local filesystem identity.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct RemoteDirectoryIdentity(String);

impl fmt::Debug for RemoteDirectoryIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RemoteDirectoryIdentity(<redacted>)")
    }
}

impl RemoteDirectoryIdentity {
    pub(crate) fn new(value: String) -> Result<Self, RemoteWorkspaceValueError> {
        let normalized = value == "/"
            || value.strip_prefix('/').is_some_and(|relative| {
                !relative.is_empty()
                    && relative.split('/').all(|component| {
                        !component.is_empty() && component != "." && component != ".."
                    })
            });
        if !normalized || value.chars().any(char::is_control) {
            return Err(RemoteWorkspaceValueError::DirectoryIdentity);
        }
        Ok(Self(value))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// The validated destination and home identity retained for a Remote Workspace connection.
///
/// Both the exact destination token and validated physical directory participate in equality.
/// The selected directory spelling is deliberately excluded and remains separate startup data.
#[derive(Clone, Eq, Hash, PartialEq)]
pub(crate) struct RemoteWorkspaceTarget {
    destination: SshDestination,
    physical_directory: RemoteDirectoryIdentity,
}

impl fmt::Debug for RemoteWorkspaceTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RemoteWorkspaceTarget(<redacted>)")
    }
}

impl RemoteWorkspaceTarget {
    pub(crate) const fn new(
        destination: SshDestination,
        physical_directory: RemoteDirectoryIdentity,
    ) -> Self {
        Self {
            destination,
            physical_directory,
        }
    }

    pub(crate) const fn destination(&self) -> &SshDestination {
        &self.destination
    }

    pub(crate) const fn physical_directory(&self) -> &RemoteDirectoryIdentity {
        &self.physical_directory
    }
}

/// Where a Workspace runs. Pinning is independent of its execution location.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WorkspaceLocation {
    Local,
    Remote {
        key: RemoteWorkspaceTarget,
        remote_directory: RemoteDirectory,
        remote_home_identity: RemoteDirectoryIdentity,
        connection_state: RemoteConnectionState,
    },
}

/// An explicit starting-directory override with authority retained on its own machine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PinnedDirectory {
    Local(ValidatedLocalDirectory),
    Remote {
        directory: RemoteDirectory,
        identity: RemoteDirectoryIdentity,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum DirectoryAvailability {
    Available,
    Unavailable { reason: String },
}

impl DirectoryAvailability {
    pub(crate) const fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }
}

#[derive(Clone, Eq)]
/// A validated local directory and its local filesystem identity.
///
/// Local launch and pin operations retain this authority when passing directories to native APIs.
pub(crate) struct ValidatedLocalDirectory {
    path: PathBuf,
    identity: LocalDirectoryIdentity,
}

impl PartialEq for ValidatedLocalDirectory {
    fn eq(&self, other: &Self) -> bool {
        self.path.as_os_str() == other.path.as_os_str() && self.identity == other.identity
    }
}

impl fmt::Debug for ValidatedLocalDirectory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ValidatedLocalDirectory(<redacted>)")
    }
}

impl ValidatedLocalDirectory {
    pub(crate) fn new(path: PathBuf, identity: LocalDirectoryIdentity) -> Self {
        Self { path, identity }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn identity(&self) -> LocalDirectoryIdentity {
        self.identity.clone()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum HomeDirectoryLocation {
    Local(ValidatedLocalDirectory),
    Remote,
}

impl fmt::Display for WorkspaceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub(crate) enum WorkspaceError {
    #[error("Workspace {0} does not belong to this collection")]
    WorkspaceNotFound(WorkspaceId),
    #[error("Workspace {0} is not a Remote Workspace")]
    RemoteConnectionUnavailable(WorkspaceId),
    #[error("Workspace {0} connection generation is exhausted")]
    RemoteConnectionGenerationExhausted(WorkspaceId),
    #[error("Workspace {0} directory belongs to a different machine")]
    DirectoryLocationMismatch(WorkspaceId),
    #[error("Workspace ID space is exhausted")]
    IdSpaceExhausted,
}

pub(crate) struct WorkspaceEntry<T> {
    id: WorkspaceId,
    name: String,
    custom_name: Option<String>,
    fallback_name: String,
    location: WorkspaceLocation,
    pinned_directory: Option<PinnedDirectory>,
    directory_location: HomeDirectoryLocation,
    availability: DirectoryAvailability,
    payload: T,
}

impl<T> WorkspaceEntry<T> {
    pub(crate) const fn id(&self) -> WorkspaceId {
        self.id
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn local_home_directory(&self) -> Option<&Path> {
        match &self.directory_location {
            HomeDirectoryLocation::Local(directory) => Some(directory.path()),
            HomeDirectoryLocation::Remote => None,
        }
    }

    pub(crate) const fn location(&self) -> &WorkspaceLocation {
        &self.location
    }

    pub(crate) const fn remote_starting_directory(&self) -> Option<&RemoteDirectory> {
        match &self.location {
            WorkspaceLocation::Remote {
                remote_directory, ..
            } => Some(remote_directory),
            WorkspaceLocation::Local => None,
        }
    }

    pub(crate) const fn remote_workspace_key(&self) -> Option<&RemoteWorkspaceTarget> {
        match &self.location {
            WorkspaceLocation::Remote { key, .. } => Some(key),
            WorkspaceLocation::Local => None,
        }
    }

    pub(crate) const fn remote_connection_state(&self) -> Option<RemoteConnectionState> {
        match &self.location {
            WorkspaceLocation::Remote {
                connection_state, ..
            } => Some(*connection_state),
            WorkspaceLocation::Local => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn directory_identity(&self) -> Option<LocalDirectoryIdentity> {
        match &self.directory_location {
            HomeDirectoryLocation::Local(directory) => Some(directory.identity()),
            HomeDirectoryLocation::Remote => None,
        }
    }

    pub(crate) fn local_display_directory(&self) -> Option<&Path> {
        match &self.pinned_directory {
            Some(PinnedDirectory::Local(directory)) => Some(directory.path()),
            _ => self.local_home_directory(),
        }
    }

    pub(crate) fn remote_display_directory(&self) -> Option<&RemoteDirectory> {
        match &self.pinned_directory {
            Some(PinnedDirectory::Remote { directory, .. }) => Some(directory),
            _ => self.remote_starting_directory(),
        }
    }

    pub(crate) const fn pinned_directory(&self) -> Option<&PinnedDirectory> {
        self.pinned_directory.as_ref()
    }

    pub(crate) const fn availability(&self) -> &DirectoryAvailability {
        &self.availability
    }

    pub(crate) const fn payload(&self) -> &T {
        &self.payload
    }
}

pub(crate) struct WorkspaceCollection<T> {
    workspaces: Vec<WorkspaceEntry<T>>,
    active_workspace_id: WorkspaceId,
    next_workspace_id: u64,
}

impl<T> WorkspaceCollection<T> {
    #[cfg(test)]
    pub(crate) fn new(
        working_directory: PathBuf,
        create_initial_payload: impl FnOnce(WorkspaceId, &Path) -> T,
    ) -> Self {
        let directory =
            ValidatedLocalDirectory::new(working_directory, LocalDirectoryIdentity::for_test(0));
        Self::new_local(directory, create_initial_payload)
    }

    pub(crate) fn new_local(
        directory: ValidatedLocalDirectory,
        create_initial_payload: impl FnOnce(WorkspaceId, &Path) -> T,
    ) -> Self {
        let id = WorkspaceId::from_raw(1);
        let payload = create_initial_payload(id, directory.path());
        Self {
            workspaces: vec![WorkspaceEntry {
                id,
                name: default_workspace_name(1),
                custom_name: None,
                fallback_name: default_workspace_name(1),
                location: WorkspaceLocation::Local,
                pinned_directory: None,
                directory_location: HomeDirectoryLocation::Local(directory),
                availability: DirectoryAvailability::Available,
                payload,
            }],
            active_workspace_id: id,
            next_workspace_id: 2,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.workspaces.len()
    }

    pub(crate) const fn active_workspace_id(&self) -> WorkspaceId {
        self.active_workspace_id
    }

    pub(crate) fn active_workspace(&self) -> &WorkspaceEntry<T> {
        let Some(workspace) = self.workspace(self.active_workspace_id) else {
            unreachable!("the Active Workspace ID must always reference an owned Workspace")
        };
        workspace
    }

    pub(crate) fn workspace(&self, workspace_id: WorkspaceId) -> Option<&WorkspaceEntry<T>> {
        self.workspaces
            .iter()
            .find(|workspace| workspace.id == workspace_id)
    }

    pub(crate) fn iter(&self) -> impl ExactSizeIterator<Item = &WorkspaceEntry<T>> {
        self.workspaces.iter()
    }

    /// Starts exactly one reconnect attempt from a disconnected or failed Remote Workspace.
    ///
    /// The collection owns checked generation allocation so callers cannot manufacture, reuse, or
    /// skip generations. Errors and illegal reductions leave the Workspace unchanged.
    pub(crate) fn begin_remote_reconnect(
        &mut self,
        workspace_id: WorkspaceId,
    ) -> Result<RemoteConnectionReduction, WorkspaceError> {
        self.remote_connection_state_mut(workspace_id)?
            .begin_reconnect()
            .ok_or(WorkspaceError::RemoteConnectionGenerationExhausted(
                workspace_id,
            ))
    }

    /// Applies one observed lifecycle transition to the owning Remote Workspace.
    ///
    /// The state's reducer rejects stale generations and illegal phase changes without mutation.
    /// Missing and non-Remote Workspace IDs return typed errors.
    pub(crate) fn reduce_remote_connection_state(
        &mut self,
        workspace_id: WorkspaceId,
        next: RemoteConnectionState,
    ) -> Result<RemoteConnectionReduction, WorkspaceError> {
        Ok(self.remote_connection_state_mut(workspace_id)?.reduce(next))
    }

    /// Begins terminal shutdown without advancing the current Connection Generation.
    ///
    /// Closing is terminal: delayed readiness, failure, disconnect, and reconnect observations
    /// cannot resurrect this Workspace. Rejection leaves the prior state unchanged.
    pub(crate) fn begin_remote_close(
        &mut self,
        workspace_id: WorkspaceId,
    ) -> Result<RemoteConnectionReduction, WorkspaceError> {
        Ok(self
            .remote_connection_state_mut(workspace_id)?
            .begin_close())
    }

    #[cfg(test)]
    pub(crate) fn create_local_workspace_unchecked(
        &mut self,
        directory: PathBuf,
        create_payload: impl FnOnce(WorkspaceId, &Path) -> T,
    ) -> Result<WorkspaceId, WorkspaceError> {
        self.create_local_workspace(
            ValidatedLocalDirectory::new(directory, LocalDirectoryIdentity::for_test(0)),
            create_payload,
        )
    }

    pub(crate) fn create_local_workspace(
        &mut self,
        directory: ValidatedLocalDirectory,
        create_payload: impl FnOnce(WorkspaceId, &Path) -> T,
    ) -> Result<WorkspaceId, WorkspaceError> {
        let (id, next) = self.next_workspace_id()?;
        let payload = create_payload(id, directory.path());
        let name = self.next_default_workspace_name(None);
        self.workspaces.push(WorkspaceEntry {
            id,
            fallback_name: name.clone(),
            name,
            custom_name: None,
            location: WorkspaceLocation::Local,
            pinned_directory: None,
            directory_location: HomeDirectoryLocation::Local(directory),
            availability: DirectoryAvailability::Available,
            payload,
        });
        self.active_workspace_id = id;
        self.next_workspace_id = next;
        self.recalculate_automatic_names();
        Ok(id)
    }

    pub(crate) fn create_remote_workspace(
        &mut self,
        key: RemoteWorkspaceTarget,
        remote_directory: RemoteDirectory,
        remote_home_identity: RemoteDirectoryIdentity,
        connection_state: RemoteConnectionState,
        create_payload: impl FnOnce(WorkspaceId) -> T,
    ) -> Result<WorkspaceId, WorkspaceError> {
        let (id, next) = self.next_workspace_id()?;
        let payload = create_payload(id);
        let base = key.destination().as_str();
        let mut name = base.to_owned();
        let mut ordinal = 2;
        while self
            .workspaces
            .iter()
            .any(|workspace| workspace.name == name)
        {
            name = format!("{base} {ordinal}");
            ordinal += 1;
        }
        self.workspaces.push(WorkspaceEntry {
            id,
            fallback_name: name.clone(),
            name,
            custom_name: None,
            location: WorkspaceLocation::Remote {
                key,
                remote_directory,
                remote_home_identity,
                connection_state,
            },
            pinned_directory: None,
            directory_location: HomeDirectoryLocation::Remote,
            availability: DirectoryAvailability::Available,
            payload,
        });
        self.active_workspace_id = id;
        self.next_workspace_id = next;
        self.recalculate_automatic_names();
        Ok(id)
    }

    pub(crate) fn set_pinned_directory(
        &mut self,
        workspace_id: WorkspaceId,
        pin: Option<PinnedDirectory>,
    ) -> Result<(), WorkspaceError> {
        let workspace = self
            .workspace_mut(workspace_id)
            .ok_or(WorkspaceError::WorkspaceNotFound(workspace_id))?;
        if matches!(
            (&workspace.location, &pin),
            (
                WorkspaceLocation::Local,
                Some(PinnedDirectory::Remote { .. })
            ) | (
                WorkspaceLocation::Remote { .. },
                Some(PinnedDirectory::Local(_))
            )
        ) {
            return Err(WorkspaceError::DirectoryLocationMismatch(workspace_id));
        }
        workspace.pinned_directory = pin;
        workspace.availability = DirectoryAvailability::Available;
        self.recalculate_automatic_names();
        Ok(())
    }

    pub(crate) fn activate_workspace(
        &mut self,
        workspace_id: WorkspaceId,
    ) -> Result<(), WorkspaceError> {
        if self.workspace(workspace_id).is_none() {
            return Err(WorkspaceError::WorkspaceNotFound(workspace_id));
        }

        self.active_workspace_id = workspace_id;
        Ok(())
    }

    pub(crate) fn rename_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        name: String,
    ) -> Result<(), WorkspaceError> {
        let workspace = self
            .workspace_mut(workspace_id)
            .ok_or(WorkspaceError::WorkspaceNotFound(workspace_id))?;
        workspace.custom_name = (!name.trim().is_empty()).then(|| name.trim().to_owned());
        self.recalculate_automatic_names();
        Ok(())
    }

    /// Freezes a creation name without colliding with another Workspace on the same machine.
    pub(crate) fn name_workspace_for_creation(
        &mut self,
        workspace_id: WorkspaceId,
        name: String,
    ) -> Result<(), WorkspaceError> {
        let workspace = self
            .workspace(workspace_id)
            .ok_or(WorkspaceError::WorkspaceNotFound(workspace_id))?;
        let base = name.trim();
        if base.is_empty() {
            return self.rename_workspace(workspace_id, name);
        }
        let occupied: std::collections::HashSet<_> = self
            .workspaces
            .iter()
            .filter(|other| {
                other.id != workspace_id
                    && match (&workspace.location, &other.location) {
                        (WorkspaceLocation::Local, WorkspaceLocation::Local) => true,
                        (
                            WorkspaceLocation::Remote { key, .. },
                            WorkspaceLocation::Remote { key: other_key, .. },
                        ) => key.destination() == other_key.destination(),
                        _ => false,
                    }
            })
            .map(|workspace| workspace.name())
            .collect();
        let mut unique_name = base.to_owned();
        let mut ordinal = 1;
        while occupied.contains(unique_name.as_str()) {
            unique_name = format!("{base} {ordinal}");
            ordinal += 1;
        }
        self.rename_workspace(workspace_id, unique_name)
    }

    pub(crate) fn set_directory_unavailable(
        &mut self,
        workspace_id: WorkspaceId,
        reason: String,
    ) -> Result<(), WorkspaceError> {
        let Some(workspace) = self.workspace_mut(workspace_id) else {
            return Err(WorkspaceError::WorkspaceNotFound(workspace_id));
        };
        workspace.availability = DirectoryAvailability::Unavailable { reason };
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn close_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        replacement_working_directory: PathBuf,
        create_replacement: impl FnOnce(WorkspaceId, &Path) -> T,
    ) -> Result<CloseWorkspaceOutcome<T>, WorkspaceError> {
        let replacement = ValidatedLocalDirectory::new(
            replacement_working_directory,
            LocalDirectoryIdentity::for_test(0),
        );
        self.close_workspace_with_replacement(workspace_id, replacement, create_replacement)
    }

    pub(crate) fn close_workspace_with_local_replacement(
        &mut self,
        workspace_id: WorkspaceId,
        replacement: ValidatedLocalDirectory,
        create_replacement: impl FnOnce(WorkspaceId, &Path) -> T,
    ) -> Result<CloseWorkspaceOutcome<T>, WorkspaceError> {
        self.close_workspace_with_replacement(workspace_id, replacement, create_replacement)
    }

    fn close_workspace_with_replacement(
        &mut self,
        workspace_id: WorkspaceId,
        replacement: ValidatedLocalDirectory,
        create_replacement: impl FnOnce(WorkspaceId, &Path) -> T,
    ) -> Result<CloseWorkspaceOutcome<T>, WorkspaceError> {
        let Some(index) = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id == workspace_id)
        else {
            return Err(WorkspaceError::WorkspaceNotFound(workspace_id));
        };

        if HierarchyClose::Workspace.resolve(self.workspaces.len()) == CloseContinuation::Replace {
            let (replacement_workspace_id, next_workspace_id) = self.next_workspace_id()?;
            let replacement_name = self.next_default_workspace_name(Some(workspace_id));
            let replacement_payload =
                create_replacement(replacement_workspace_id, replacement.path());
            let closed_workspace = std::mem::replace(
                &mut self.workspaces[index],
                WorkspaceEntry {
                    id: replacement_workspace_id,
                    fallback_name: replacement_name.clone(),
                    name: replacement_name,
                    custom_name: None,
                    location: WorkspaceLocation::Local,
                    pinned_directory: None,
                    directory_location: HomeDirectoryLocation::Local(replacement),
                    availability: DirectoryAvailability::Available,
                    payload: replacement_payload,
                },
            );
            self.active_workspace_id = replacement_workspace_id;
            self.next_workspace_id = next_workspace_id;

            return Ok(CloseWorkspaceOutcome::FinalWorkspaceReplaced {
                closed_workspace_id: closed_workspace.id,
                replacement_workspace_id,
                payload: closed_workspace.payload,
            });
        }

        let closed_workspace = self.workspaces.remove(index);
        if self.active_workspace_id == workspace_id {
            let fallback_index = index.min(self.workspaces.len() - 1);
            self.active_workspace_id = self.workspaces[fallback_index].id;
        }

        self.recalculate_automatic_names();
        Ok(CloseWorkspaceOutcome::WorkspaceClosed {
            closed_workspace_id: closed_workspace.id,
            active_workspace_id: self.active_workspace_id,
            payload: closed_workspace.payload,
        })
    }

    pub(crate) fn close_workspace_for_final_tab(
        &mut self,
        workspace_id: WorkspaceId,
    ) -> Result<FinalTabCloseOutcome<T>, WorkspaceError> {
        let Some(index) = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id == workspace_id)
        else {
            return Err(WorkspaceError::WorkspaceNotFound(workspace_id));
        };

        if HierarchyClose::FinalTab.resolve(self.workspaces.len()) == CloseContinuation::Window {
            return Ok(FinalTabCloseOutcome::CloseOperatingSystemWindow { workspace_id });
        }

        let closed_workspace = self.workspaces.remove(index);
        if self.active_workspace_id == workspace_id {
            let fallback_index = index.min(self.workspaces.len() - 1);
            self.active_workspace_id = self.workspaces[fallback_index].id;
        }

        self.recalculate_automatic_names();
        Ok(FinalTabCloseOutcome::WorkspaceClosed {
            closed_workspace_id: closed_workspace.id,
            active_workspace_id: self.active_workspace_id,
            payload: closed_workspace.payload,
        })
    }

    fn workspace_mut(&mut self, workspace_id: WorkspaceId) -> Option<&mut WorkspaceEntry<T>> {
        self.workspaces
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)
    }

    fn remote_connection_state_mut(
        &mut self,
        workspace_id: WorkspaceId,
    ) -> Result<&mut RemoteConnectionState, WorkspaceError> {
        let Some(workspace) = self.workspace_mut(workspace_id) else {
            return Err(WorkspaceError::WorkspaceNotFound(workspace_id));
        };
        let WorkspaceLocation::Remote {
            connection_state, ..
        } = &mut workspace.location
        else {
            return Err(WorkspaceError::RemoteConnectionUnavailable(workspace_id));
        };
        Ok(connection_state)
    }

    fn next_workspace_id(&self) -> Result<(WorkspaceId, u64), WorkspaceError> {
        let value = self.next_workspace_id;
        let next = value
            .checked_add(1)
            .ok_or(WorkspaceError::IdSpaceExhausted)?;
        Ok((WorkspaceId::from_raw(value), next))
    }

    fn next_default_workspace_name(&self, excluded_workspace_id: Option<WorkspaceId>) -> String {
        for workspace_number in 1..=self.workspaces.len().saturating_add(1) {
            let candidate = default_workspace_name(workspace_number);
            let is_available = self.workspaces.iter().all(|workspace| {
                Some(workspace.id) == excluded_workspace_id || workspace.name != candidate
            });
            if is_available {
                return candidate;
            }
        }

        unreachable!("one of len + 1 default Workspace names must be available")
    }
}

fn default_workspace_name(workspace_number: usize) -> String {
    format!("Workspace {workspace_number}")
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    struct DropProbe {
        drops: Rc<Cell<usize>>,
    }

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.drops.update(|drops| drops + 1);
        }
    }

    fn new_workspaces<T>(payload: T) -> WorkspaceCollection<T> {
        WorkspaceCollection::new(PathBuf::from("/first"), |_, working_directory| {
            assert_eq!(working_directory, Path::new("/first"));
            payload
        })
    }

    fn validated(path: &str, identity_label: u64) -> ValidatedLocalDirectory {
        ValidatedLocalDirectory::new(
            PathBuf::from(path),
            LocalDirectoryIdentity::for_test(identity_label),
        )
    }

    fn ssh_destination(value: &str) -> SshDestination {
        SshDestination::new(value.to_owned()).unwrap()
    }

    fn remote_directory(value: &str) -> RemoteDirectory {
        RemoteDirectory::new(value.to_owned()).unwrap()
    }

    #[test]
    fn remote_identity_debug_should_redact_destination_and_directories() {
        let destination = ssh_destination("sensitive-host");
        let selected = remote_directory("/sensitive/selected");
        let physical = RemoteDirectoryIdentity::new("/sensitive/physical".to_owned()).unwrap();
        let key = RemoteWorkspaceTarget::new(destination.clone(), physical.clone());
        let local = ValidatedLocalDirectory::new(
            PathBuf::from("/sensitive/local"),
            LocalDirectoryIdentity::for_test(1001),
        );

        for debug in [
            format!("{destination:?}"),
            format!("{selected:?}"),
            format!("{physical:?}"),
            format!("{key:?}"),
            format!("{local:?}"),
        ] {
            assert!(!debug.contains("sensitive"));
        }
    }

    fn remote_identity(value: &str) -> RemoteDirectoryIdentity {
        RemoteDirectoryIdentity::new(value.to_owned()).unwrap()
    }

    fn remote_key(destination: &str, physical_directory: &str) -> RemoteWorkspaceTarget {
        RemoteWorkspaceTarget::new(
            ssh_destination(destination),
            remote_identity(physical_directory),
        )
    }

    #[test]
    fn remote_values_validate_without_changing_user_visible_spelling() {
        let destination = ssh_destination("root@fedora@orb");
        let directory = remote_directory("~/Projects/Space Term/");
        let identity = remote_identity("/home/root/Projects/Space Term");

        assert_eq!(
            (destination.as_str(), directory.as_str(), identity.as_str(),),
            (
                "root@fedora@orb",
                "~/Projects/Space Term/",
                "/home/root/Projects/Space Term",
            )
        );
        assert!(SshDestination::new("bad destination".to_owned()).is_err());
        assert!(RemoteDirectory::new("relative/path".to_owned()).is_err());
        assert!(RemoteDirectoryIdentity::new("~/not-physical".to_owned()).is_err());
    }

    #[test]
    fn remote_physical_identity_requires_normalized_absolute_form() {
        for invalid in [
            "",
            "relative/path",
            "//srv/project",
            "/srv//project",
            "/srv/./project",
            "/srv/team/../project",
            "/srv/project/",
        ] {
            assert!(
                RemoteDirectoryIdentity::new(invalid.to_owned()).is_err(),
                "{invalid:?} is not normalized `pwd -P` output"
            );
        }

        assert_eq!(remote_identity("/").as_str(), "/");
        assert_eq!(
            remote_identity("/srv/Space Term").as_str(),
            "/srv/Space Term"
        );
    }

    #[test]
    fn remote_directories_never_provide_local_filesystem_authority() {
        let mut workspaces =
            WorkspaceCollection::new_local(validated("/Users/test", 10), |_, _| ());
        let workspace_id = workspaces
            .create_remote_workspace(
                remote_key("orb", "/home/test/project"),
                remote_directory("~/project"),
                remote_identity("/home/test"),
                RemoteConnectionState::connected(1),
                |_| (),
            )
            .unwrap();
        let workspace = workspaces.workspace(workspace_id).unwrap();

        assert_eq!(workspace.local_home_directory(), None);
        assert_eq!(workspace.directory_identity(), None);
        assert_eq!(
            workspace
                .remote_starting_directory()
                .map(RemoteDirectory::as_str),
            Some("~/project")
        );
    }

    #[test]
    fn remote_workspace_key_keeps_different_destination_aliases_distinct() {
        let mut workspaces =
            WorkspaceCollection::new_local(validated("/Users/test", 10), |_, _| ());
        let first = workspaces
            .create_remote_workspace(
                remote_key("orb", "/srv/project"),
                remote_directory("/srv/project"),
                remote_identity("/home/test"),
                RemoteConnectionState::connected(1),
                |_| (),
            )
            .unwrap();
        let second = workspaces
            .create_remote_workspace(
                remote_key("orb-alias", "/srv/project"),
                remote_directory("/srv/project"),
                remote_identity("/home/test"),
                RemoteConnectionState::connected(1),
                |_| (),
            )
            .unwrap();

        assert_ne!(first, second);
        assert_eq!(workspaces.len(), 3);
    }

    #[test]
    fn remote_connection_state_rejects_stale_generations() {
        let mut state = RemoteConnectionState::reconnecting(7);

        assert_eq!(
            state.reduce(RemoteConnectionState::connected(7)),
            RemoteConnectionReduction::Applied
        );
        assert_eq!(
            state.reduce(RemoteConnectionState::failed(6)),
            RemoteConnectionReduction::Stale
        );
        assert_eq!(state, RemoteConnectionState::connected(7));
        assert_eq!(state.generation(), 7);
    }

    #[test]
    fn closing_remote_connection_state_is_terminal() {
        let mut state = RemoteConnectionState::closing(9);

        assert_eq!(
            state.reduce(RemoteConnectionState::connected(9)),
            RemoteConnectionReduction::Illegal
        );
        assert_eq!(
            state.reduce(RemoteConnectionState::reconnecting(10)),
            RemoteConnectionReduction::Illegal
        );
        assert_eq!(state, RemoteConnectionState::closing(9));
    }

    #[test]
    fn reconnect_requires_a_new_generation_and_rejects_predecessor_completions() {
        let mut state = RemoteConnectionState::disconnected(7);

        assert_eq!(
            state.reduce(RemoteConnectionState::reconnecting(7)),
            RemoteConnectionReduction::Illegal
        );
        assert_eq!(
            state.reduce(RemoteConnectionState::reconnecting(8)),
            RemoteConnectionReduction::Applied
        );
        assert_eq!(
            state.reduce(RemoteConnectionState::connected(7)),
            RemoteConnectionReduction::Stale
        );
        assert_eq!(
            state.reduce(RemoteConnectionState::connected(8)),
            RemoteConnectionReduction::Applied
        );
        assert_eq!(
            state.reduce(RemoteConnectionState::disconnected(7)),
            RemoteConnectionReduction::Stale
        );
        assert_eq!(state, RemoteConnectionState::connected(8));
    }

    #[test]
    fn delayed_ready_cannot_resurrect_a_disconnected_generation() {
        let mut state = RemoteConnectionState::connected(4);

        assert_eq!(
            state.reduce(RemoteConnectionState::disconnected(4)),
            RemoteConnectionReduction::Applied
        );
        assert_eq!(
            state.reduce(RemoteConnectionState::connected(4)),
            RemoteConnectionReduction::Illegal
        );
        assert_eq!(state, RemoteConnectionState::disconnected(4));
    }

    #[test]
    fn remote_connection_operations_reject_missing_and_local_workspaces_without_mutation() {
        let mut workspaces =
            WorkspaceCollection::new_local(validated("/Users/test", 10), |_, _| ());
        let local_id = workspaces
            .create_local_workspace(validated("/Users/test/project", 20), |_, _| ())
            .unwrap();
        let before = workspaces
            .iter()
            .map(|workspace| (workspace.id(), workspace.location().clone()))
            .collect::<Vec<_>>();

        for workspace_id in [WorkspaceId::new(1), local_id] {
            assert_eq!(
                workspaces.begin_remote_reconnect(workspace_id),
                Err(WorkspaceError::RemoteConnectionUnavailable(workspace_id))
            );
            assert_eq!(
                workspaces.reduce_remote_connection_state(
                    workspace_id,
                    RemoteConnectionState::disconnected(1),
                ),
                Err(WorkspaceError::RemoteConnectionUnavailable(workspace_id))
            );
            assert_eq!(
                workspaces.begin_remote_close(workspace_id),
                Err(WorkspaceError::RemoteConnectionUnavailable(workspace_id))
            );
        }
        let missing = WorkspaceId::new(999);
        assert_eq!(
            workspaces.begin_remote_reconnect(missing),
            Err(WorkspaceError::WorkspaceNotFound(missing))
        );
        assert_eq!(
            workspaces
                .reduce_remote_connection_state(missing, RemoteConnectionState::disconnected(1),),
            Err(WorkspaceError::WorkspaceNotFound(missing))
        );
        assert_eq!(
            workspaces.begin_remote_close(missing),
            Err(WorkspaceError::WorkspaceNotFound(missing))
        );
        assert_eq!(
            workspaces
                .iter()
                .map(|workspace| (workspace.id(), workspace.location().clone()))
                .collect::<Vec<_>>(),
            before
        );
    }

    #[test]
    fn reconnect_generation_exhaustion_does_not_mutate_remote_state() {
        let mut workspaces =
            WorkspaceCollection::new_local(validated("/Users/test", 10), |_, _| ());
        let workspace_id = workspaces
            .create_remote_workspace(
                remote_key("orb", "/srv/project"),
                remote_directory("/srv/project"),
                remote_identity("/home/test"),
                RemoteConnectionState::disconnected(u64::MAX),
                |_| (),
            )
            .unwrap();

        assert_eq!(
            workspaces.begin_remote_reconnect(workspace_id),
            Err(WorkspaceError::RemoteConnectionGenerationExhausted(
                workspace_id
            ))
        );
        assert_eq!(
            workspaces
                .workspace(workspace_id)
                .and_then(WorkspaceEntry::remote_connection_state),
            Some(RemoteConnectionState::disconnected(u64::MAX))
        );
    }

    #[test]
    fn begin_remote_reconnect_advances_once_and_rejects_a_second_begin() {
        let mut workspaces =
            WorkspaceCollection::new_local(validated("/Users/test", 10), |_, _| ());
        let workspace_id = workspaces
            .create_remote_workspace(
                remote_key("orb", "/srv/project"),
                remote_directory("/srv/project"),
                remote_identity("/home/test"),
                RemoteConnectionState::disconnected(7),
                |_| (),
            )
            .unwrap();

        assert_eq!(
            workspaces.begin_remote_reconnect(workspace_id),
            Ok(RemoteConnectionReduction::Applied)
        );
        assert_eq!(
            workspaces.begin_remote_reconnect(workspace_id),
            Ok(RemoteConnectionReduction::Illegal)
        );
        assert_eq!(
            workspaces
                .workspace(workspace_id)
                .and_then(WorkspaceEntry::remote_connection_state),
            Some(RemoteConnectionState::reconnecting(8))
        );
    }

    #[test]
    fn begin_remote_reconnect_rejects_connected_state_without_advancing() {
        let mut workspaces =
            WorkspaceCollection::new_local(validated("/Users/test", 10), |_, _| ());
        let workspace_id = workspaces
            .create_remote_workspace(
                remote_key("orb", "/srv/project"),
                remote_directory("/srv/project"),
                remote_identity("/home/test"),
                RemoteConnectionState::connected(7),
                |_| (),
            )
            .unwrap();

        assert_eq!(
            workspaces.begin_remote_reconnect(workspace_id),
            Ok(RemoteConnectionReduction::Illegal)
        );
        assert_eq!(
            workspaces
                .workspace(workspace_id)
                .and_then(WorkspaceEntry::remote_connection_state),
            Some(RemoteConnectionState::connected(7))
        );
    }

    #[test]
    fn begin_remote_close_accepts_each_live_phase_without_advancing_generation() {
        for (index, state) in [
            RemoteConnectionState::connected(4),
            RemoteConnectionState::reconnecting(5),
            RemoteConnectionState::disconnected(6),
            RemoteConnectionState::failed(7),
        ]
        .into_iter()
        .enumerate()
        {
            let mut workspaces =
                WorkspaceCollection::new_local(validated("/Users/test", 10), |_, _| ());
            let workspace_id = workspaces
                .create_remote_workspace(
                    remote_key(&format!("orb-{index}"), "/srv/project"),
                    remote_directory("/srv/project"),
                    remote_identity("/home/test"),
                    state,
                    |_| (),
                )
                .unwrap();

            assert_eq!(
                workspaces.begin_remote_close(workspace_id),
                Ok(RemoteConnectionReduction::Applied)
            );
            assert_eq!(
                workspaces
                    .workspace(workspace_id)
                    .and_then(WorkspaceEntry::remote_connection_state),
                Some(RemoteConnectionState::closing(state.generation()))
            );
        }
    }

    #[test]
    fn collection_reducer_preserves_stale_and_illegal_results_without_mutation() {
        let mut workspaces =
            WorkspaceCollection::new_local(validated("/Users/test", 10), |_, _| ());
        let workspace_id = workspaces
            .create_remote_workspace(
                remote_key("orb", "/srv/project"),
                remote_directory("/srv/project"),
                remote_identity("/home/test"),
                RemoteConnectionState::disconnected(7),
                |_| (),
            )
            .unwrap();
        assert_eq!(
            workspaces.begin_remote_reconnect(workspace_id),
            Ok(RemoteConnectionReduction::Applied)
        );

        assert_eq!(
            workspaces
                .reduce_remote_connection_state(workspace_id, RemoteConnectionState::failed(7),),
            Ok(RemoteConnectionReduction::Stale)
        );
        assert_eq!(
            workspaces.reduce_remote_connection_state(
                workspace_id,
                RemoteConnectionState::reconnecting(9),
            ),
            Ok(RemoteConnectionReduction::Illegal)
        );
        assert_eq!(
            workspaces
                .workspace(workspace_id)
                .and_then(WorkspaceEntry::remote_connection_state),
            Some(RemoteConnectionState::reconnecting(8))
        );
    }

    #[test]
    fn collection_remote_lifecycle_reduces_legal_transitions_and_closing_is_terminal() {
        let mut workspaces =
            WorkspaceCollection::new_local(validated("/Users/test", 10), |_, _| ());
        let workspace_id = workspaces
            .create_remote_workspace(
                remote_key("orb", "/srv/project"),
                remote_directory("/srv/project"),
                remote_identity("/home/test"),
                RemoteConnectionState::connected(4),
                |_| (),
            )
            .unwrap();

        assert_eq!(
            workspaces.reduce_remote_connection_state(
                workspace_id,
                RemoteConnectionState::disconnected(4),
            ),
            Ok(RemoteConnectionReduction::Applied)
        );
        assert_eq!(
            workspaces.begin_remote_reconnect(workspace_id),
            Ok(RemoteConnectionReduction::Applied)
        );
        assert_eq!(
            workspaces
                .reduce_remote_connection_state(workspace_id, RemoteConnectionState::failed(5),),
            Ok(RemoteConnectionReduction::Applied)
        );
        assert_eq!(
            workspaces.begin_remote_reconnect(workspace_id),
            Ok(RemoteConnectionReduction::Applied)
        );
        assert_eq!(
            workspaces
                .reduce_remote_connection_state(workspace_id, RemoteConnectionState::connected(6),),
            Ok(RemoteConnectionReduction::Applied)
        );
        assert_eq!(
            workspaces.begin_remote_close(workspace_id),
            Ok(RemoteConnectionReduction::Applied)
        );
        assert_eq!(
            workspaces.begin_remote_close(workspace_id),
            Ok(RemoteConnectionReduction::Illegal)
        );
        assert_eq!(
            workspaces.reduce_remote_connection_state(
                workspace_id,
                RemoteConnectionState::disconnected(6),
            ),
            Ok(RemoteConnectionReduction::Illegal)
        );
        assert_eq!(
            workspaces
                .workspace(workspace_id)
                .and_then(WorkspaceEntry::remote_connection_state),
            Some(RemoteConnectionState::closing(6))
        );
    }

    #[test]
    fn pins_are_explicit_independent_and_do_not_change_home() {
        let mut workspaces = WorkspaceCollection::new_local(validated("/home/me", 1), |_, _| ());
        let first = workspaces.active_workspace_id();
        let second = workspaces
            .create_local_workspace(validated("/home/me", 1), |_, _| ())
            .unwrap();
        let pin = PinnedDirectory::Local(validated("/app/frontend", 2));
        workspaces
            .set_pinned_directory(first, Some(pin.clone()))
            .unwrap();
        workspaces
            .set_pinned_directory(second, Some(pin.clone()))
            .unwrap();
        assert_ne!(first, second);
        assert_eq!(workspaces.len(), 2);
        let first_workspace = workspaces.workspace(first).unwrap();
        assert_eq!(first_workspace.name(), "frontend");
        assert_eq!(
            first_workspace.local_home_directory(),
            Some(Path::new("/home/me"))
        );
        assert_eq!(first_workspace.pinned_directory(), Some(&pin));
        workspaces.set_pinned_directory(first, None).unwrap();
        assert!(
            workspaces
                .workspace(first)
                .unwrap()
                .pinned_directory()
                .is_none()
        );
        assert_eq!(
            workspaces.workspace(second).unwrap().pinned_directory(),
            Some(&pin)
        );
    }

    #[test]
    fn remote_workspaces_at_the_same_home_are_independent_and_pins_keep_remote_authority() {
        let mut workspaces = WorkspaceCollection::new_local(validated("/home/me", 1), |_, _| ());
        let mut create = || {
            workspaces
                .create_remote_workspace(
                    remote_key("server", "/home/me"),
                    remote_directory("~/"),
                    remote_identity("/home/me"),
                    RemoteConnectionState::connected(1),
                    |_| (),
                )
                .unwrap()
        };
        let first = create();
        let second = create();
        assert_ne!(first, second);
        assert_eq!(workspaces.workspace(first).unwrap().name(), "server");
        assert_eq!(workspaces.workspace(second).unwrap().name(), "server 2");
        let remote_pin = PinnedDirectory::Remote {
            directory: remote_directory("/app"),
            identity: remote_identity("/app"),
        };
        workspaces
            .set_pinned_directory(first, Some(remote_pin.clone()))
            .unwrap();
        assert_eq!(
            workspaces.workspace(first).unwrap().pinned_directory(),
            Some(&remote_pin)
        );
        assert_eq!(
            workspaces
                .workspace(first)
                .unwrap()
                .local_display_directory(),
            None
        );
        assert_eq!(
            workspaces
                .set_pinned_directory(first, Some(PinnedDirectory::Local(validated("/app", 2)))),
            Err(WorkspaceError::DirectoryLocationMismatch(first))
        );
        assert_eq!(
            workspaces.set_pinned_directory(WorkspaceId::new(1), Some(remote_pin)),
            Err(WorkspaceError::DirectoryLocationMismatch(WorkspaceId::new(
                1
            )))
        );
    }

    #[test]
    fn new_should_create_one_valid_active_workspace() {
        let workspaces = new_workspaces("first payload");

        assert_eq!(
            (
                workspaces.len(),
                workspaces.active_workspace_id(),
                workspaces.active_workspace().id(),
                workspaces.active_workspace().payload(),
            ),
            (
                1,
                WorkspaceId::new(1),
                WorkspaceId::new(1),
                &"first payload",
            )
        );
    }

    #[test]
    fn iter_should_preserve_workspace_creation_order() {
        let mut workspaces = new_workspaces("first payload");
        workspaces
            .create_local_workspace_unchecked(PathBuf::from("/second"), |_, _| "second payload")
            .unwrap();
        workspaces
            .create_local_workspace_unchecked(PathBuf::from("/third"), |_, _| "third payload")
            .unwrap();

        let ordered_ids = workspaces
            .iter()
            .map(WorkspaceEntry::id)
            .collect::<Vec<_>>();

        assert_eq!(
            ordered_ids,
            vec![
                WorkspaceId::new(1),
                WorkspaceId::new(2),
                WorkspaceId::new(3),
            ]
        );
    }

    #[test]
    fn create_local_workspace_unchecked_should_create_and_activate_the_new_workspace() {
        let mut workspaces = new_workspaces("first payload");

        let created = workspaces
            .create_local_workspace_unchecked(PathBuf::from("/second"), |_, _| "second payload")
            .unwrap();

        assert_eq!(
            (
                created,
                workspaces.len(),
                workspaces.active_workspace_id(),
                workspaces.active_workspace().payload(),
            ),
            (
                WorkspaceId::new(2),
                2,
                WorkspaceId::new(2),
                &"second payload",
            )
        );
    }

    #[test]
    fn create_local_workspace_unchecked_should_choose_the_first_available_default_name() {
        let mut workspaces = new_workspaces("first payload");
        workspaces
            .create_local_workspace_unchecked(PathBuf::from("/second"), |_, _| "second payload")
            .unwrap();
        workspaces
            .rename_workspace(WorkspaceId::new(1), "Projects".to_owned())
            .unwrap();

        workspaces
            .create_local_workspace_unchecked(PathBuf::from("/third"), |_, _| "third payload")
            .unwrap();

        let names = workspaces
            .iter()
            .map(WorkspaceEntry::name)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["Projects", "Workspace 2", "Workspace 1"]);
    }

    #[test]
    fn create_local_workspace_unchecked_should_assign_its_name_and_propagate_the_exact_working_directory()
     {
        let mut workspaces = new_workspaces("first payload");
        let working_directory = PathBuf::from("/Users/test/projects");
        let observed_pointer = Cell::new(std::ptr::null());

        workspaces
            .create_local_workspace_unchecked(working_directory, |_, payload_working_directory| {
                observed_pointer.set(
                    payload_working_directory
                        .as_os_str()
                        .as_encoded_bytes()
                        .as_ptr(),
                );
                "second payload"
            })
            .unwrap();

        let stored_working_directory = workspaces
            .active_workspace()
            .local_home_directory()
            .unwrap();
        assert_eq!(
            (
                workspaces.active_workspace().name(),
                stored_working_directory,
            ),
            ("Workspace 2", Path::new("/Users/test/projects"))
        );
        assert_eq!(
            observed_pointer.get(),
            stored_working_directory
                .as_os_str()
                .as_encoded_bytes()
                .as_ptr(),
            "payload construction must borrow the PathBuf stored by the Workspace",
        );
    }

    #[test]
    fn create_local_workspace_unchecked_should_reject_exhausted_ids_before_creating_its_payload() {
        let mut workspaces = new_workspaces("first payload");
        workspaces.next_workspace_id = u64::MAX;
        let creations = Cell::new(0);

        let result =
            workspaces.create_local_workspace_unchecked(PathBuf::from("/second"), |_, _| {
                creations.update(|count| count + 1);
                "second payload"
            });

        assert_eq!(
            (result, creations.get(), workspaces.len()),
            (Err(WorkspaceError::IdSpaceExhausted), 0, 1,)
        );
    }

    #[test]
    fn activate_workspace_should_select_an_owned_workspace() {
        let mut workspaces = new_workspaces("first payload");
        workspaces
            .create_local_workspace_unchecked(PathBuf::from("/second"), |_, _| "second payload")
            .unwrap();

        workspaces.activate_workspace(WorkspaceId::new(1)).unwrap();

        assert_eq!(
            (
                workspaces.active_workspace_id(),
                workspaces.active_workspace().payload(),
            ),
            (WorkspaceId::new(1), &"first payload")
        );
    }

    #[test]
    fn activate_workspace_should_reject_an_unknown_id_without_mutation() {
        let mut workspaces = new_workspaces("first payload");

        let result = workspaces.activate_workspace(WorkspaceId::new(99));

        assert_eq!(
            (result, workspaces.active_workspace_id()),
            (
                Err(WorkspaceError::WorkspaceNotFound(WorkspaceId::new(99))),
                WorkspaceId::new(1),
            )
        );
    }

    #[test]
    fn creation_name_should_fill_suffix_gaps_without_renumbering_frozen_names() {
        let mut workspaces = new_workspaces(());
        let first = workspaces.active_workspace_id();
        workspaces
            .rename_workspace(first, "Project".to_owned())
            .unwrap();
        let second = workspaces
            .create_local_workspace_unchecked(PathBuf::from("/second"), |_, _| ())
            .unwrap();
        workspaces
            .rename_workspace(second, "Project 2".to_owned())
            .unwrap();
        for directory in ["/third", "/fourth"] {
            let id = workspaces
                .create_local_workspace_unchecked(PathBuf::from(directory), |_, _| ())
                .unwrap();
            workspaces
                .name_workspace_for_creation(id, " Project ".to_owned())
                .unwrap();
        }

        assert_eq!(
            workspaces
                .iter()
                .map(WorkspaceEntry::name)
                .collect::<Vec<_>>(),
            ["Project", "Project 2", "Project 1", "Project 3"]
        );
    }

    #[test]
    fn creation_name_should_avoid_automatic_names_and_remain_frozen_after_directory_changes() {
        let mut workspaces = new_workspaces(());
        let automatic_name = workspaces.active_workspace().name().to_owned();
        let id = workspaces
            .create_local_workspace_unchecked(PathBuf::from("/second"), |_, _| ())
            .unwrap();
        workspaces
            .name_workspace_for_creation(id, automatic_name.clone())
            .unwrap();
        workspaces
            .set_pinned_directory(
                id,
                Some(PinnedDirectory::Local(validated("/different/project", 90))),
            )
            .unwrap();

        let workspace = workspaces.workspace(id).unwrap();
        let expected = format!("{automatic_name} 1");
        assert_eq!(
            (workspace.name(), workspace.custom_name.as_deref()),
            (expected.as_str(), Some(expected.as_str()))
        );
    }

    #[test]
    fn creation_name_should_exclude_its_own_displayed_name() {
        let mut workspaces = new_workspaces(());
        let id = workspaces.active_workspace_id();
        let name = workspaces.active_workspace().name().to_owned();
        workspaces
            .name_workspace_for_creation(id, name.clone())
            .unwrap();

        assert_eq!(
            workspaces.active_workspace().custom_name.as_deref(),
            Some(name.as_str())
        );
    }

    #[test]
    fn creation_name_should_scope_remote_collisions_to_the_exact_destination() {
        let mut workspaces = new_workspaces(());
        let local = workspaces.active_workspace_id();
        workspaces
            .name_workspace_for_creation(local, "Project".to_owned())
            .unwrap();
        let mut assigned = Vec::new();
        for (destination, directory) in [
            ("orb", "/srv/first"),
            ("orb", "/srv/second"),
            ("orb-alias", "/srv/first"),
        ] {
            let id = workspaces
                .create_remote_workspace(
                    remote_key(destination, directory),
                    remote_directory(directory),
                    remote_identity("/home/test"),
                    RemoteConnectionState::connected(1),
                    |_| (),
                )
                .unwrap();
            workspaces
                .name_workspace_for_creation(id, "Project".to_owned())
                .unwrap();
            assigned.push(workspaces.workspace(id).unwrap().name().to_owned());
        }
        let local_second = workspaces
            .create_local_workspace_unchecked(PathBuf::from("/second"), |_, _| ())
            .unwrap();
        workspaces
            .name_workspace_for_creation(local_second, "Project".to_owned())
            .unwrap();
        assigned.push(
            workspaces
                .workspace(local_second)
                .unwrap()
                .name()
                .to_owned(),
        );

        assert_eq!(assigned, ["Project", "Project 1", "Project", "Project 1"]);
    }

    #[test]
    fn creation_name_should_keep_blank_names_automatic() {
        let mut workspaces = new_workspaces(());
        let id = workspaces.active_workspace_id();
        workspaces
            .name_workspace_for_creation(id, " \t ".to_owned())
            .unwrap();

        assert_eq!(workspaces.active_workspace().custom_name, None);
    }

    #[test]
    fn creation_name_should_reject_an_unknown_workspace_without_mutation() {
        let mut workspaces = new_workspaces(());
        let name = workspaces.active_workspace().name().to_owned();
        let result = workspaces.name_workspace_for_creation(WorkspaceId::new(99), name.clone());

        assert_eq!(
            (result, workspaces.active_workspace().name()),
            (
                Err(WorkspaceError::WorkspaceNotFound(WorkspaceId::new(99))),
                name.as_str()
            )
        );
    }

    #[test]
    fn creation_name_should_preserve_case_sensitive_matching_and_manual_rename_semantics() {
        let mut workspaces = new_workspaces(());
        let first = workspaces.active_workspace_id();
        workspaces
            .rename_workspace(first, "Project".to_owned())
            .unwrap();
        let second = workspaces
            .create_local_workspace_unchecked(PathBuf::from("/second"), |_, _| ())
            .unwrap();
        workspaces
            .name_workspace_for_creation(second, "project".to_owned())
            .unwrap();
        assert_eq!(workspaces.workspace(second).unwrap().name(), "project");

        workspaces
            .rename_workspace(second, "Project".to_owned())
            .unwrap();
        assert_eq!(
            workspaces
                .iter()
                .map(WorkspaceEntry::name)
                .collect::<Vec<_>>(),
            ["Project", "Project"]
        );
    }

    #[test]
    fn rename_workspace_should_update_only_the_requested_workspace_name() {
        let mut workspaces = new_workspaces("first payload");
        workspaces
            .create_local_workspace_unchecked(PathBuf::from("/second"), |_, _| "second payload")
            .unwrap();

        workspaces
            .rename_workspace(WorkspaceId::new(1), "renamed".to_owned())
            .unwrap();

        let names = workspaces
            .iter()
            .map(WorkspaceEntry::name)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["renamed", "Workspace 2"]);
    }

    #[test]
    fn rename_workspace_should_reject_an_unknown_id_without_mutation() {
        let mut workspaces = new_workspaces("first payload");

        let result = workspaces.rename_workspace(WorkspaceId::new(99), "unknown".to_owned());

        assert_eq!(
            (result, workspaces.active_workspace().name()),
            (
                Err(WorkspaceError::WorkspaceNotFound(WorkspaceId::new(99))),
                "Workspace 1",
            )
        );
    }

    #[test]
    fn non_final_close_should_not_allocate_a_replacement_workspace_id() {
        let mut workspaces = new_workspaces("first payload");
        workspaces
            .create_local_workspace_unchecked(PathBuf::from("/second"), |_, _| "second payload")
            .unwrap();
        workspaces.next_workspace_id = u64::MAX;

        let outcome = workspaces
            .close_workspace(
                WorkspaceId::new(1),
                PathBuf::from("/replacement"),
                |_, _| unreachable!("a replacement is not needed"),
            )
            .unwrap();

        assert_eq!(
            outcome,
            CloseWorkspaceOutcome::WorkspaceClosed {
                closed_workspace_id: WorkspaceId::new(1),
                active_workspace_id: WorkspaceId::new(2),
                payload: "first payload",
            }
        );
    }

    #[test]
    fn closed_workspace_ids_should_not_be_reused() {
        let mut workspaces = new_workspaces("first payload");
        let second = workspaces
            .create_local_workspace_unchecked(PathBuf::from("/second"), |_, _| "second payload")
            .unwrap();
        workspaces
            .close_workspace(second, PathBuf::from("/replacement"), |_, _| {
                unreachable!("a replacement is not needed")
            })
            .unwrap();

        let third = workspaces
            .create_local_workspace_unchecked(PathBuf::from("/third"), |_, _| "third payload")
            .unwrap();

        assert_eq!(third, WorkspaceId::new(3));
    }

    #[test]
    fn close_workspace_should_focus_the_next_workspace_when_closing_the_active_middle_workspace() {
        let mut workspaces = new_workspaces("first payload");
        workspaces
            .create_local_workspace_unchecked(PathBuf::from("/second"), |_, _| "second payload")
            .unwrap();
        workspaces
            .create_local_workspace_unchecked(PathBuf::from("/third"), |_, _| "third payload")
            .unwrap();
        workspaces.activate_workspace(WorkspaceId::new(2)).unwrap();

        let outcome = workspaces
            .close_workspace(
                WorkspaceId::new(2),
                PathBuf::from("/replacement"),
                |_, _| unreachable!("a replacement is not needed"),
            )
            .unwrap();

        let CloseWorkspaceOutcome::WorkspaceClosed {
            active_workspace_id,
            ..
        } = outcome
        else {
            panic!("closing one of multiple Workspaces must remove it")
        };
        assert_eq!(active_workspace_id, WorkspaceId::new(3));
    }

    #[test]
    fn close_workspace_should_focus_the_previous_workspace_when_closing_the_active_last_workspace()
    {
        let mut workspaces = new_workspaces("first payload");
        workspaces
            .create_local_workspace_unchecked(PathBuf::from("/second"), |_, _| "second payload")
            .unwrap();

        let outcome = workspaces
            .close_workspace(
                WorkspaceId::new(2),
                PathBuf::from("/replacement"),
                |_, _| unreachable!("a replacement is not needed"),
            )
            .unwrap();

        let CloseWorkspaceOutcome::WorkspaceClosed {
            active_workspace_id,
            ..
        } = outcome
        else {
            panic!("closing one of multiple Workspaces must remove it")
        };
        assert_eq!(active_workspace_id, WorkspaceId::new(1));
    }

    #[test]
    fn close_workspace_should_reject_an_unknown_id_without_factory_side_effects_or_mutation() {
        let mut workspaces = new_workspaces("first payload");
        let creations = Cell::new(0);

        let result = workspaces.close_workspace(
            WorkspaceId::new(99),
            PathBuf::from("/replacement"),
            |_, _| {
                creations.update(|count| count + 1);
                "replacement payload"
            },
        );

        assert_eq!(
            (
                result.err(),
                creations.get(),
                workspaces.len(),
                workspaces.active_workspace_id(),
            ),
            (
                Some(WorkspaceError::WorkspaceNotFound(WorkspaceId::new(99))),
                0,
                1,
                WorkspaceId::new(1),
            )
        );
    }

    #[test]
    fn close_workspace_should_atomically_replace_and_activate_the_final_workspace() {
        let mut workspaces = new_workspaces("first payload");
        let observed_pointer = Cell::new(std::ptr::null());

        let outcome = workspaces
            .close_workspace(
                WorkspaceId::new(1),
                PathBuf::from("/replacement"),
                |_, working_directory| {
                    observed_pointer.set(working_directory.as_os_str().as_encoded_bytes().as_ptr());
                    "replacement payload"
                },
            )
            .unwrap();

        let replacement_working_directory = workspaces
            .active_workspace()
            .local_home_directory()
            .unwrap();

        assert_eq!(
            (
                outcome,
                workspaces.len(),
                workspaces.active_workspace_id(),
                workspaces.active_workspace().id(),
                workspaces.active_workspace().name(),
                replacement_working_directory,
                workspaces.active_workspace().payload(),
            ),
            (
                CloseWorkspaceOutcome::FinalWorkspaceReplaced {
                    closed_workspace_id: WorkspaceId::new(1),
                    replacement_workspace_id: WorkspaceId::new(2),
                    payload: "first payload",
                },
                1,
                WorkspaceId::new(2),
                WorkspaceId::new(2),
                "Workspace 1",
                Path::new("/replacement"),
                &"replacement payload",
            )
        );
        assert_eq!(
            observed_pointer.get(),
            replacement_working_directory
                .as_os_str()
                .as_encoded_bytes()
                .as_ptr(),
            "replacement construction must borrow the PathBuf stored by the Workspace",
        );
    }

    #[test]
    fn final_tab_close_should_remove_a_non_final_workspace_without_allocating_a_replacement() {
        let mut workspaces = new_workspaces("first payload");
        workspaces
            .create_local_workspace_unchecked(PathBuf::from("/second"), |_, _| "second payload")
            .unwrap();
        workspaces.next_workspace_id = u64::MAX;

        let outcome = workspaces
            .close_workspace_for_final_tab(WorkspaceId::new(1))
            .unwrap();

        assert_eq!(
            outcome,
            FinalTabCloseOutcome::WorkspaceClosed {
                closed_workspace_id: WorkspaceId::new(1),
                active_workspace_id: WorkspaceId::new(2),
                payload: "first payload",
            }
        );
        assert_eq!(workspaces.active_workspace().payload(), &"second payload");
    }

    #[test]
    fn final_tab_close_should_preserve_the_globally_final_workspace() {
        let mut workspaces = new_workspaces("first payload");
        workspaces.next_workspace_id = u64::MAX;

        let outcome = workspaces
            .close_workspace_for_final_tab(WorkspaceId::new(1))
            .unwrap();

        assert_eq!(
            outcome,
            FinalTabCloseOutcome::CloseOperatingSystemWindow {
                workspace_id: WorkspaceId::new(1),
            }
        );
        assert_eq!(
            (
                workspaces.len(),
                workspaces.active_workspace_id(),
                workspaces.active_workspace().payload(),
            ),
            (1, WorkspaceId::new(1), &"first payload")
        );
    }

    #[test]
    fn final_tab_close_should_transfer_ownership_and_drop_each_payload_exactly_once() {
        let drops = Rc::new(Cell::new(0));
        let mut workspaces = new_workspaces(DropProbe {
            drops: Rc::clone(&drops),
        });
        workspaces
            .create_local_workspace_unchecked(PathBuf::from("/second"), |_, _| DropProbe {
                drops: Rc::clone(&drops),
            })
            .unwrap();

        let outcome = workspaces
            .close_workspace_for_final_tab(WorkspaceId::new(1))
            .unwrap();
        assert_eq!(drops.get(), 0);

        drop(outcome);
        assert_eq!(drops.get(), 1);

        drop(workspaces);
        assert_eq!(drops.get(), 2);
    }

    #[test]
    fn close_workspace_should_reject_exhausted_ids_before_factory_side_effects() {
        let mut workspaces = new_workspaces("first payload");
        workspaces.next_workspace_id = u64::MAX;
        let creations = Cell::new(0);

        let result = workspaces.close_workspace(
            WorkspaceId::new(1),
            PathBuf::from("/replacement"),
            |_, _| {
                creations.update(|count| count + 1);
                "replacement payload"
            },
        );

        assert_eq!(
            (
                result.err(),
                creations.get(),
                workspaces.len(),
                workspaces.active_workspace_id(),
            ),
            (
                Some(WorkspaceError::IdSpaceExhausted),
                0,
                1,
                WorkspaceId::new(1),
            )
        );
    }

    #[test]
    fn close_workspace_should_transfer_ownership_and_drop_each_payload_exactly_once() {
        let drops = Rc::new(Cell::new(0));
        let mut workspaces = new_workspaces(DropProbe {
            drops: Rc::clone(&drops),
        });

        let outcome = workspaces
            .close_workspace(
                WorkspaceId::new(1),
                PathBuf::from("/replacement"),
                |_, _| DropProbe {
                    drops: Rc::clone(&drops),
                },
            )
            .unwrap();
        assert_eq!(drops.get(), 0);

        drop(outcome);
        assert_eq!(drops.get(), 1);

        drop(workspaces);
        assert_eq!(drops.get(), 2);
    }
}
