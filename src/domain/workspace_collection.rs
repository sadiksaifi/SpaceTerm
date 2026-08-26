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

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct WorkspaceDirectoryIdentity {
    device: u64,
    file: u64,
}

impl WorkspaceDirectoryIdentity {
    pub(crate) const fn new(device: u64, file: u64) -> Self {
        Self { device, file }
    }

    #[cfg(test)]
    pub(crate) const fn is_synthetic(self) -> bool {
        self.device == 0 && self.file == 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DirectoryAuthority {
    window_id: super::WindowId,
    pane_id: super::PaneId,
}

impl DirectoryAuthority {
    pub(crate) const fn initial() -> Self {
        Self::new(super::WindowId::from_raw(1), super::PaneId::from_raw(1))
    }

    pub(crate) const fn new(window_id: super::WindowId, pane_id: super::PaneId) -> Self {
        Self { window_id, pane_id }
    }

    pub(crate) const fn window_id(self) -> super::WindowId {
        self.window_id
    }

    #[cfg(test)]
    pub(crate) const fn pane_id(self) -> super::PaneId {
        self.pane_id
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WorkspaceKind {
    Scratch {
        directory_authority: DirectoryAuthority,
    },
    LocalProject {
        project_root_identity: WorkspaceDirectoryIdentity,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WorkspaceDirectoryAvailability {
    Available,
    Unavailable { reason: String },
}

impl WorkspaceDirectoryAvailability {
    pub(crate) const fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ValidatedWorkspaceDirectory {
    path: PathBuf,
    identity: WorkspaceDirectoryIdentity,
}

impl ValidatedWorkspaceDirectory {
    pub(crate) fn new(path: PathBuf, identity: WorkspaceDirectoryIdentity) -> Self {
        Self { path, identity }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) const fn identity(&self) -> WorkspaceDirectoryIdentity {
        self.identity
    }
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
    #[error("Workspace ID space is exhausted")]
    IdSpaceExhausted,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum CloseWorkspaceOutcome<T> {
    WorkspaceClosed {
        closed_workspace_id: WorkspaceId,
        active_workspace_id: WorkspaceId,
        payload: T,
    },
    FinalWorkspaceReplaced {
        closed_workspace_id: WorkspaceId,
        replacement_workspace_id: WorkspaceId,
        payload: T,
    },
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum FinalWindowCloseOutcome<T> {
    WorkspaceClosed {
        closed_workspace_id: WorkspaceId,
        active_workspace_id: WorkspaceId,
        payload: T,
    },
    CloseOperatingSystemWindow {
        workspace_id: WorkspaceId,
    },
}

pub(crate) struct WorkspaceEntry<T> {
    id: WorkspaceId,
    name: String,
    custom_name: Option<String>,
    kind: WorkspaceKind,
    working_directory: PathBuf,
    directory_identity: WorkspaceDirectoryIdentity,
    availability: WorkspaceDirectoryAvailability,
    payload: T,
}

impl<T> WorkspaceEntry<T> {
    pub(crate) const fn id(&self) -> WorkspaceId {
        self.id
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn working_directory(&self) -> &Path {
        &self.working_directory
    }

    pub(crate) const fn kind(&self) -> &WorkspaceKind {
        &self.kind
    }

    pub(crate) const fn directory_identity(&self) -> WorkspaceDirectoryIdentity {
        self.directory_identity
    }

    pub(crate) const fn availability(&self) -> &WorkspaceDirectoryAvailability {
        &self.availability
    }

    #[cfg(test)]
    pub(crate) fn custom_name(&self) -> Option<&str> {
        self.custom_name.as_deref()
    }

    pub(crate) const fn payload(&self) -> &T {
        &self.payload
    }
}

pub(crate) struct WorkspaceCollection<T> {
    workspaces: Vec<WorkspaceEntry<T>>,
    active_workspace_id: WorkspaceId,
    next_workspace_id: u64,
    home_identity: WorkspaceDirectoryIdentity,
    directory_names: bool,
}

impl<T> WorkspaceCollection<T> {
    #[cfg(test)]
    pub(crate) fn new(
        working_directory: PathBuf,
        create_initial_payload: impl FnOnce(WorkspaceId, &Path) -> T,
    ) -> Self {
        let directory = ValidatedWorkspaceDirectory::new(
            working_directory,
            WorkspaceDirectoryIdentity::new(0, 0),
        );
        let mut collection = Self::new_scratch(
            directory,
            DirectoryAuthority::initial(),
            create_initial_payload,
        );
        collection.directory_names = false;
        collection.workspaces[0].name = default_workspace_name(1);
        collection
    }

    pub(crate) fn new_scratch(
        directory: ValidatedWorkspaceDirectory,
        directory_authority: DirectoryAuthority,
        create_initial_payload: impl FnOnce(WorkspaceId, &Path) -> T,
    ) -> Self {
        let initial_workspace_id = WorkspaceId::from_raw(1);
        let payload = create_initial_payload(initial_workspace_id, directory.path());
        let mut collection = Self {
            workspaces: vec![WorkspaceEntry {
                id: initial_workspace_id,
                name: String::new(),
                custom_name: None,
                kind: WorkspaceKind::Scratch {
                    directory_authority,
                },
                working_directory: directory.path,
                directory_identity: directory.identity,
                availability: WorkspaceDirectoryAvailability::Available,
                payload,
            }],
            active_workspace_id: initial_workspace_id,
            next_workspace_id: 2,
            home_identity: directory.identity,
            directory_names: true,
        };
        collection.recalculate_automatic_names();
        collection
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

    pub(crate) fn local_project_workspace(
        &self,
        identity: WorkspaceDirectoryIdentity,
    ) -> Option<WorkspaceId> {
        self.workspaces.iter().find_map(|workspace| {
            matches!(
                workspace.kind,
                WorkspaceKind::LocalProject {
                    project_root_identity
                } if project_root_identity == identity
            )
            .then_some(workspace.id)
        })
    }

    #[cfg(test)]
    pub(crate) fn create_workspace(
        &mut self,
        working_directory: PathBuf,
        create_payload: impl FnOnce(WorkspaceId, &Path) -> T,
    ) -> Result<WorkspaceId, WorkspaceError> {
        let (workspace_id, next_workspace_id) = self.next_workspace_id()?;
        let name = self.next_default_workspace_name(None);
        let payload = create_payload(workspace_id, &working_directory);
        self.workspaces.push(WorkspaceEntry {
            id: workspace_id,
            name: name.clone(),
            custom_name: Some(name),
            kind: WorkspaceKind::Scratch {
                directory_authority: DirectoryAuthority::initial(),
            },
            working_directory,
            directory_identity: WorkspaceDirectoryIdentity::new(0, workspace_id.get()),
            availability: WorkspaceDirectoryAvailability::Available,
            payload,
        });
        self.active_workspace_id = workspace_id;
        self.next_workspace_id = next_workspace_id;
        Ok(workspace_id)
    }

    pub(crate) fn create_scratch_workspace(
        &mut self,
        directory: ValidatedWorkspaceDirectory,
        directory_authority: DirectoryAuthority,
        create_payload: impl FnOnce(WorkspaceId, &Path) -> T,
    ) -> Result<WorkspaceId, WorkspaceError> {
        self.create_workspace_entry(
            WorkspaceKind::Scratch {
                directory_authority,
            },
            directory,
            create_payload,
        )
    }

    pub(crate) fn create_local_project_workspace(
        &mut self,
        directory: ValidatedWorkspaceDirectory,
        create_payload: impl FnOnce(WorkspaceId, &Path) -> T,
    ) -> Result<WorkspaceId, WorkspaceError> {
        if let Some(existing_id) = self.local_project_workspace(directory.identity) {
            self.active_workspace_id = existing_id;
            return Ok(existing_id);
        }
        let kind = WorkspaceKind::LocalProject {
            project_root_identity: directory.identity,
        };
        self.create_workspace_entry(kind, directory, create_payload)
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
        let directory_names = self.directory_names;
        let Some(workspace) = self.workspace_mut(workspace_id) else {
            return Err(WorkspaceError::WorkspaceNotFound(workspace_id));
        };

        let custom_name = (!name.trim().is_empty()).then(|| name.trim().to_owned());
        workspace.custom_name = custom_name.clone();
        if !directory_names && let Some(custom_name) = custom_name {
            workspace.name = custom_name;
        }
        self.recalculate_automatic_names();
        Ok(())
    }

    pub(crate) fn update_directory_authority_report(
        &mut self,
        workspace_id: WorkspaceId,
        authority: DirectoryAuthority,
        directory: ValidatedWorkspaceDirectory,
    ) -> Result<bool, WorkspaceError> {
        let Some(workspace) = self.workspace_mut(workspace_id) else {
            return Err(WorkspaceError::WorkspaceNotFound(workspace_id));
        };
        let WorkspaceKind::Scratch {
            directory_authority,
        } = &workspace.kind
        else {
            return Ok(false);
        };
        if *directory_authority != authority {
            return Ok(false);
        }
        let changed = workspace.working_directory != directory.path
            || workspace.directory_identity != directory.identity
            || !workspace.availability.is_available();
        workspace.working_directory = directory.path;
        workspace.directory_identity = directory.identity;
        workspace.availability = WorkspaceDirectoryAvailability::Available;
        self.recalculate_automatic_names();
        Ok(changed)
    }

    pub(crate) fn mark_directory_authority_unavailable(
        &mut self,
        workspace_id: WorkspaceId,
        authority: DirectoryAuthority,
        reason: String,
    ) -> Result<bool, WorkspaceError> {
        let Some(workspace) = self.workspace_mut(workspace_id) else {
            return Err(WorkspaceError::WorkspaceNotFound(workspace_id));
        };
        let WorkspaceKind::Scratch {
            directory_authority,
        } = &workspace.kind
        else {
            return Ok(false);
        };
        if *directory_authority != authority {
            return Ok(false);
        }
        workspace.availability = WorkspaceDirectoryAvailability::Unavailable { reason };
        Ok(true)
    }

    pub(crate) fn promote_directory_authority(
        &mut self,
        workspace_id: WorkspaceId,
        removed_authority: DirectoryAuthority,
        promoted_authority: DirectoryAuthority,
        directory: Option<ValidatedWorkspaceDirectory>,
    ) -> Result<bool, WorkspaceError> {
        let Some(workspace) = self.workspace_mut(workspace_id) else {
            return Err(WorkspaceError::WorkspaceNotFound(workspace_id));
        };
        let WorkspaceKind::Scratch {
            directory_authority,
        } = &mut workspace.kind
        else {
            return Ok(false);
        };
        if *directory_authority != removed_authority {
            return Ok(false);
        }
        *directory_authority = promoted_authority;
        if let Some(directory) = directory {
            workspace.working_directory = directory.path;
            workspace.directory_identity = directory.identity;
            workspace.availability = WorkspaceDirectoryAvailability::Available;
        }
        self.recalculate_automatic_names();
        Ok(true)
    }

    pub(crate) fn promote_directory_authority_for_window(
        &mut self,
        workspace_id: WorkspaceId,
        removed_window_id: super::WindowId,
        promoted_authority: DirectoryAuthority,
        directory: Option<ValidatedWorkspaceDirectory>,
    ) -> Result<bool, WorkspaceError> {
        let Some(workspace) = self.workspace_mut(workspace_id) else {
            return Err(WorkspaceError::WorkspaceNotFound(workspace_id));
        };
        let WorkspaceKind::Scratch {
            directory_authority,
        } = &mut workspace.kind
        else {
            return Ok(false);
        };
        if directory_authority.window_id() != removed_window_id {
            return Ok(false);
        }
        *directory_authority = promoted_authority;
        if let Some(directory) = directory {
            workspace.working_directory = directory.path;
            workspace.directory_identity = directory.identity;
            workspace.availability = WorkspaceDirectoryAvailability::Available;
        }
        self.recalculate_automatic_names();
        Ok(true)
    }

    pub(crate) fn set_directory_unavailable(
        &mut self,
        workspace_id: WorkspaceId,
        reason: String,
    ) -> Result<(), WorkspaceError> {
        let Some(workspace) = self.workspace_mut(workspace_id) else {
            return Err(WorkspaceError::WorkspaceNotFound(workspace_id));
        };
        workspace.availability = WorkspaceDirectoryAvailability::Unavailable { reason };
        Ok(())
    }

    pub(crate) fn set_directory_available(
        &mut self,
        workspace_id: WorkspaceId,
        identity: WorkspaceDirectoryIdentity,
    ) -> Result<(), WorkspaceError> {
        let Some(workspace) = self.workspace_mut(workspace_id) else {
            return Err(WorkspaceError::WorkspaceNotFound(workspace_id));
        };
        workspace.directory_identity = identity;
        workspace.availability = WorkspaceDirectoryAvailability::Available;
        self.recalculate_automatic_names();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn close_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        replacement_working_directory: PathBuf,
        create_replacement: impl FnOnce(WorkspaceId, &Path) -> T,
    ) -> Result<CloseWorkspaceOutcome<T>, WorkspaceError> {
        let replacement =
            ValidatedWorkspaceDirectory::new(replacement_working_directory, self.home_identity);
        self.close_workspace_with_replacement(
            workspace_id,
            replacement,
            DirectoryAuthority::initial(),
            create_replacement,
        )
    }

    pub(crate) fn close_workspace_with_scratch_replacement(
        &mut self,
        workspace_id: WorkspaceId,
        replacement: ValidatedWorkspaceDirectory,
        directory_authority: DirectoryAuthority,
        create_replacement: impl FnOnce(WorkspaceId, &Path) -> T,
    ) -> Result<CloseWorkspaceOutcome<T>, WorkspaceError> {
        self.close_workspace_with_replacement(
            workspace_id,
            replacement,
            directory_authority,
            create_replacement,
        )
    }

    fn close_workspace_with_replacement(
        &mut self,
        workspace_id: WorkspaceId,
        replacement: ValidatedWorkspaceDirectory,
        directory_authority: DirectoryAuthority,
        create_replacement: impl FnOnce(WorkspaceId, &Path) -> T,
    ) -> Result<CloseWorkspaceOutcome<T>, WorkspaceError> {
        let Some(index) = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id == workspace_id)
        else {
            return Err(WorkspaceError::WorkspaceNotFound(workspace_id));
        };

        if self.workspaces.len() == 1 {
            let (replacement_workspace_id, next_workspace_id) = self.next_workspace_id()?;
            let replacement_name = if self.directory_names {
                automatic_workspace_basename(
                    replacement.path(),
                    replacement.identity,
                    self.home_identity,
                )
            } else {
                self.next_default_workspace_name(Some(workspace_id))
            };
            let replacement_payload =
                create_replacement(replacement_workspace_id, replacement.path());
            let closed_workspace = std::mem::replace(
                &mut self.workspaces[index],
                WorkspaceEntry {
                    id: replacement_workspace_id,
                    name: replacement_name,
                    custom_name: None,
                    kind: WorkspaceKind::Scratch {
                        directory_authority,
                    },
                    working_directory: replacement.path,
                    directory_identity: replacement.identity,
                    availability: WorkspaceDirectoryAvailability::Available,
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

    pub(crate) fn close_workspace_for_final_window(
        &mut self,
        workspace_id: WorkspaceId,
    ) -> Result<FinalWindowCloseOutcome<T>, WorkspaceError> {
        let Some(index) = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id == workspace_id)
        else {
            return Err(WorkspaceError::WorkspaceNotFound(workspace_id));
        };

        if self.workspaces.len() == 1 {
            return Ok(FinalWindowCloseOutcome::CloseOperatingSystemWindow { workspace_id });
        }

        let closed_workspace = self.workspaces.remove(index);
        if self.active_workspace_id == workspace_id {
            let fallback_index = index.min(self.workspaces.len() - 1);
            self.active_workspace_id = self.workspaces[fallback_index].id;
        }
        self.recalculate_automatic_names();

        Ok(FinalWindowCloseOutcome::WorkspaceClosed {
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

    fn next_workspace_id(&self) -> Result<(WorkspaceId, u64), WorkspaceError> {
        let value = self.next_workspace_id;
        let next = value
            .checked_add(1)
            .ok_or(WorkspaceError::IdSpaceExhausted)?;
        Ok((WorkspaceId::from_raw(value), next))
    }

    fn create_workspace_entry(
        &mut self,
        kind: WorkspaceKind,
        directory: ValidatedWorkspaceDirectory,
        create_payload: impl FnOnce(WorkspaceId, &Path) -> T,
    ) -> Result<WorkspaceId, WorkspaceError> {
        let (workspace_id, next_workspace_id) = self.next_workspace_id()?;
        let payload = create_payload(workspace_id, directory.path());
        self.workspaces.push(WorkspaceEntry {
            id: workspace_id,
            name: String::new(),
            custom_name: None,
            kind,
            working_directory: directory.path,
            directory_identity: directory.identity,
            availability: WorkspaceDirectoryAvailability::Available,
            payload,
        });
        self.active_workspace_id = workspace_id;
        self.next_workspace_id = next_workspace_id;
        self.recalculate_automatic_names();
        Ok(workspace_id)
    }

    fn recalculate_automatic_names(&mut self) {
        if !self.directory_names {
            return;
        }
        let names = self
            .workspaces
            .iter()
            .enumerate()
            .map(|(index, workspace)| {
                if let Some(custom_name) = &workspace.custom_name {
                    return custom_name.clone();
                }
                let base = automatic_workspace_basename(
                    &workspace.working_directory,
                    workspace.directory_identity,
                    self.home_identity,
                );
                if !matches!(workspace.kind, WorkspaceKind::Scratch { .. }) {
                    return base;
                }
                let ordinal = self.workspaces[..index]
                    .iter()
                    .filter(|candidate| {
                        candidate.custom_name.is_none()
                            && matches!(candidate.kind, WorkspaceKind::Scratch { .. })
                            && candidate.directory_identity == workspace.directory_identity
                    })
                    .count()
                    + 1;
                if ordinal == 1 {
                    base
                } else {
                    format!("{base} {ordinal}")
                }
            })
            .collect::<Vec<_>>();
        for (workspace, name) in self.workspaces.iter_mut().zip(names) {
            workspace.name = name;
        }
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

fn automatic_workspace_basename(
    path: &Path,
    identity: WorkspaceDirectoryIdentity,
    home_identity: WorkspaceDirectoryIdentity,
) -> String {
    if identity == home_identity {
        return "Default".to_owned();
    }
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "/".to_owned())
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

    fn validated(path: &str, file: u64) -> ValidatedWorkspaceDirectory {
        ValidatedWorkspaceDirectory::new(
            PathBuf::from(path),
            WorkspaceDirectoryIdentity::new(1, file),
        )
    }

    #[test]
    fn workspace_kinds_own_distinct_directory_policies() {
        let authority =
            DirectoryAuthority::new(super::super::WindowId::new(4), super::super::PaneId::new(7));
        let mut workspaces = WorkspaceCollection::new_scratch(
            validated("/Users/test", 10),
            authority,
            |_, _| "scratch",
        );
        let project_id = workspaces
            .create_local_project_workspace(validated("/Users/test/project", 20), |_, _| "project")
            .unwrap();

        assert!(matches!(
            workspaces.workspace(WorkspaceId::new(1)).unwrap().kind(),
            WorkspaceKind::Scratch { directory_authority } if *directory_authority == authority
        ));
        assert!(matches!(
            workspaces.workspace(project_id).unwrap().kind(),
            WorkspaceKind::LocalProject { project_root_identity }
                if *project_root_identity == WorkspaceDirectoryIdentity::new(1, 20)
        ));
        assert_eq!(
            (authority.window_id(), authority.pane_id()),
            (super::super::WindowId::new(4), super::super::PaneId::new(7))
        );
    }

    #[test]
    fn automatic_names_number_only_matching_unrenamed_scratch_workspaces() {
        let mut workspaces = WorkspaceCollection::new_scratch(
            validated("/Users/test", 10),
            DirectoryAuthority::initial(),
            |_, _| (),
        );
        let second = workspaces
            .create_scratch_workspace(
                validated("/private/alternate-home-spelling", 10),
                DirectoryAuthority::initial(),
                |_, _| (),
            )
            .unwrap();
        workspaces
            .create_local_project_workspace(validated("/Users/test", 10), |_, _| ())
            .unwrap();

        assert_eq!(
            workspaces
                .iter()
                .map(WorkspaceEntry::name)
                .collect::<Vec<_>>(),
            vec!["Default", "Default 2", "Default"]
        );
        workspaces
            .rename_workspace(second, "  Focus  ".to_owned())
            .unwrap();
        assert_eq!(
            workspaces.workspace(second).unwrap().custom_name(),
            Some("Focus")
        );
        workspaces
            .rename_workspace(second, "   ".to_owned())
            .unwrap();
        assert_eq!(
            (
                workspaces.workspace(second).unwrap().custom_name(),
                workspaces.workspace(second).unwrap().name(),
            ),
            (None, "Default 2")
        );
    }

    #[test]
    fn local_project_identity_deduplicates_without_merging_scratch() {
        let mut workspaces = WorkspaceCollection::new_scratch(
            validated("/selected/project", 20),
            DirectoryAuthority::initial(),
            |_, _| "scratch",
        );
        let created = workspaces
            .create_local_project_workspace(validated("/selected/project", 20), |_, _| "project")
            .unwrap();
        let duplicate = workspaces
            .create_local_project_workspace(validated("/equivalent/project", 20), |_, _| {
                panic!("an equivalent Local Project must not create another payload")
            })
            .unwrap();

        assert_eq!(
            (workspaces.len(), created, duplicate),
            (2, created, created)
        );
        assert_eq!(
            workspaces.workspace(created).unwrap().working_directory(),
            Path::new("/selected/project")
        );
    }

    #[test]
    fn authority_reports_validate_ownership_and_promotion() {
        let first =
            DirectoryAuthority::new(super::super::WindowId::new(1), super::super::PaneId::new(1));
        let promoted =
            DirectoryAuthority::new(super::super::WindowId::new(1), super::super::PaneId::new(2));
        let mut workspaces =
            WorkspaceCollection::new_scratch(validated("/previous", 10), first, |_, _| ());

        assert!(
            !workspaces
                .update_directory_authority_report(
                    WorkspaceId::new(1),
                    promoted,
                    validated("/ignored", 11)
                )
                .unwrap()
        );
        assert!(
            workspaces
                .mark_directory_authority_unavailable(
                    WorkspaceId::new(1),
                    first,
                    "missing".to_owned()
                )
                .unwrap()
        );
        assert!(
            workspaces
                .promote_directory_authority(
                    WorkspaceId::new(1),
                    first,
                    promoted,
                    Some(validated("/promoted", 12)),
                )
                .unwrap()
        );
        let workspace = workspaces.workspace(WorkspaceId::new(1)).unwrap();
        assert_eq!(workspace.working_directory(), Path::new("/promoted"));
        assert_eq!(
            workspace.availability(),
            &WorkspaceDirectoryAvailability::Available
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
            .create_workspace(PathBuf::from("/second"), |_, _| "second payload")
            .unwrap();
        workspaces
            .create_workspace(PathBuf::from("/third"), |_, _| "third payload")
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
    fn create_workspace_should_create_and_activate_the_new_workspace() {
        let mut workspaces = new_workspaces("first payload");

        let created = workspaces
            .create_workspace(PathBuf::from("/second"), |_, _| "second payload")
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
    fn create_workspace_should_choose_the_first_available_default_name() {
        let mut workspaces = new_workspaces("first payload");
        workspaces
            .create_workspace(PathBuf::from("/second"), |_, _| "second payload")
            .unwrap();
        workspaces
            .rename_workspace(WorkspaceId::new(1), "Projects".to_owned())
            .unwrap();

        workspaces
            .create_workspace(PathBuf::from("/third"), |_, _| "third payload")
            .unwrap();

        let names = workspaces
            .iter()
            .map(WorkspaceEntry::name)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["Projects", "Workspace 2", "Workspace 1"]);
    }

    #[test]
    fn create_workspace_should_assign_its_name_and_propagate_the_exact_working_directory() {
        let mut workspaces = new_workspaces("first payload");
        let working_directory = PathBuf::from("/Users/test/projects");
        let observed_pointer = Cell::new(std::ptr::null());

        workspaces
            .create_workspace(working_directory, |_, payload_working_directory| {
                observed_pointer.set(
                    payload_working_directory
                        .as_os_str()
                        .as_encoded_bytes()
                        .as_ptr(),
                );
                "second payload"
            })
            .unwrap();

        let stored_working_directory = workspaces.active_workspace().working_directory();
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
    fn create_workspace_should_reject_exhausted_ids_before_creating_its_payload() {
        let mut workspaces = new_workspaces("first payload");
        workspaces.next_workspace_id = u64::MAX;
        let creations = Cell::new(0);

        let result = workspaces.create_workspace(PathBuf::from("/second"), |_, _| {
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
            .create_workspace(PathBuf::from("/second"), |_, _| "second payload")
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
    fn rename_workspace_should_update_only_the_requested_workspace_name() {
        let mut workspaces = new_workspaces("first payload");
        workspaces
            .create_workspace(PathBuf::from("/second"), |_, _| "second payload")
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
            .create_workspace(PathBuf::from("/second"), |_, _| "second payload")
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
            .create_workspace(PathBuf::from("/second"), |_, _| "second payload")
            .unwrap();
        workspaces
            .close_workspace(second, PathBuf::from("/replacement"), |_, _| {
                unreachable!("a replacement is not needed")
            })
            .unwrap();

        let third = workspaces
            .create_workspace(PathBuf::from("/third"), |_, _| "third payload")
            .unwrap();

        assert_eq!(third, WorkspaceId::new(3));
    }

    #[test]
    fn close_workspace_should_focus_the_next_workspace_when_closing_the_active_middle_workspace() {
        let mut workspaces = new_workspaces("first payload");
        workspaces
            .create_workspace(PathBuf::from("/second"), |_, _| "second payload")
            .unwrap();
        workspaces
            .create_workspace(PathBuf::from("/third"), |_, _| "third payload")
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
            .create_workspace(PathBuf::from("/second"), |_, _| "second payload")
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

        let replacement_working_directory = workspaces.active_workspace().working_directory();

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
    fn final_window_close_should_remove_a_non_final_workspace_without_allocating_a_replacement() {
        let mut workspaces = new_workspaces("first payload");
        workspaces
            .create_workspace(PathBuf::from("/second"), |_, _| "second payload")
            .unwrap();
        workspaces.next_workspace_id = u64::MAX;

        let outcome = workspaces
            .close_workspace_for_final_window(WorkspaceId::new(1))
            .unwrap();

        assert_eq!(
            outcome,
            FinalWindowCloseOutcome::WorkspaceClosed {
                closed_workspace_id: WorkspaceId::new(1),
                active_workspace_id: WorkspaceId::new(2),
                payload: "first payload",
            }
        );
        assert_eq!(workspaces.active_workspace().payload(), &"second payload");
    }

    #[test]
    fn final_window_close_should_preserve_the_globally_final_workspace() {
        let mut workspaces = new_workspaces("first payload");
        workspaces.next_workspace_id = u64::MAX;

        let outcome = workspaces
            .close_workspace_for_final_window(WorkspaceId::new(1))
            .unwrap();

        assert_eq!(
            outcome,
            FinalWindowCloseOutcome::CloseOperatingSystemWindow {
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
    fn final_window_close_should_transfer_ownership_and_drop_each_payload_exactly_once() {
        let drops = Rc::new(Cell::new(0));
        let mut workspaces = new_workspaces(DropProbe {
            drops: Rc::clone(&drops),
        });
        workspaces
            .create_workspace(PathBuf::from("/second"), |_, _| DropProbe {
                drops: Rc::clone(&drops),
            })
            .unwrap();

        let outcome = workspaces
            .close_workspace_for_final_window(WorkspaceId::new(1))
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
