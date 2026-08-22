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

impl fmt::Display for WorkspaceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WorkspaceKind {
    AdHoc,
    LocalProject,
}

/// Pure data token produced by the platform layer from stat(2) dev+ino;
/// defined here so the domain stays free of syscalls.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct CanonicalDirectoryId(u64, u64);

impl CanonicalDirectoryId {
    pub(crate) const fn new(device: u64, inode: u64) -> Self {
        Self(device, inode)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DirectoryValidity {
    Valid,
    Invalid,
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "callers construct Workspace launches in a later migration"
    )
)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WorkspaceLaunch {
    AdHoc { home: PathBuf },
    LocalProject { project_root: PathBuf },
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum OpenLocalProjectOutcome<T> {
    Created {
        workspace_id: WorkspaceId,
        payload: T,
    },
    ActivatedExisting {
        existing_workspace_id: WorkspaceId,
    },
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub(crate) enum WorkspaceError {
    #[error("Workspace {0} does not belong to this collection")]
    WorkspaceNotFound(WorkspaceId),
    #[error("Workspace {0}'s directory is unavailable")]
    DirectoryUnavailable(WorkspaceId),
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
    kind: WorkspaceKind,
    custom_name: Option<String>,
    /// Some exactly while the Workspace is a Local Project; immutable after creation.
    project_root: Option<PathBuf>,
    directory: PathBuf,
    directory_available: bool,
    payload: T,
}

impl<T> WorkspaceEntry<T> {
    pub(crate) const fn id(&self) -> WorkspaceId {
        self.id
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
    pub(crate) fn new(
        home: PathBuf,
        create_initial_payload: impl FnOnce(WorkspaceId, &Path) -> T,
    ) -> Self {
        let initial_workspace_id = WorkspaceId::from_raw(1);
        let payload = create_initial_payload(initial_workspace_id, &home);
        Self {
            workspaces: vec![WorkspaceEntry {
                id: initial_workspace_id,
                kind: WorkspaceKind::AdHoc,
                custom_name: None,
                project_root: None,
                directory: home,
                directory_available: true,
                payload,
            }],
            active_workspace_id: initial_workspace_id,
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

    pub(crate) fn create_workspace(
        &mut self,
        launch: WorkspaceLaunch,
        create_payload: impl FnOnce(WorkspaceId, &Path) -> T,
    ) -> Result<WorkspaceId, WorkspaceError> {
        let (workspace_id, next_workspace_id) = self.next_workspace_id()?;
        match launch {
            WorkspaceLaunch::AdHoc { home } => {
                let payload = create_payload(workspace_id, &home);
                self.workspaces.push(WorkspaceEntry {
                    id: workspace_id,
                    kind: WorkspaceKind::AdHoc,
                    custom_name: None,
                    project_root: None,
                    directory: home,
                    directory_available: true,
                    payload,
                });
            }
            WorkspaceLaunch::LocalProject { project_root } => {
                let payload = create_payload(workspace_id, &project_root);
                self.workspaces.push(WorkspaceEntry {
                    id: workspace_id,
                    kind: WorkspaceKind::LocalProject,
                    custom_name: None,
                    project_root: Some(project_root.clone()),
                    directory: project_root,
                    directory_available: true,
                    payload,
                });
            }
        }
        self.active_workspace_id = workspace_id;
        self.next_workspace_id = next_workspace_id;
        Ok(workspace_id)
    }

    pub(crate) fn open_local_project(
        &mut self,
        selected_path: PathBuf,
        identity: CanonicalDirectoryId,
        identity_of_existing: impl Fn(&Path) -> Option<CanonicalDirectoryId>,
        create_payload: impl FnOnce(WorkspaceId, &Path) -> T,
    ) -> Result<OpenLocalProjectOutcome<T>, WorkspaceError>
    where
        T: Clone,
    {
        let existing_project = self
            .workspaces
            .iter()
            .filter(|workspace| workspace.kind == WorkspaceKind::LocalProject)
            .find(|workspace| {
                workspace
                    .project_root
                    .as_deref()
                    .is_some_and(|root| identity_of_existing(root) == Some(identity))
            })
            .map(|workspace| workspace.id);

        if let Some(existing_workspace_id) = existing_project {
            self.activate_workspace(existing_workspace_id)?;
            return Ok(OpenLocalProjectOutcome::ActivatedExisting {
                existing_workspace_id,
            });
        }

        let (workspace_id, next_workspace_id) = self.next_workspace_id()?;
        let payload = create_payload(workspace_id, &selected_path);
        self.workspaces.push(WorkspaceEntry {
            id: workspace_id,
            kind: WorkspaceKind::LocalProject,
            custom_name: None,
            project_root: Some(selected_path.clone()),
            directory: selected_path,
            directory_available: true,
            payload: payload.clone(),
        });
        self.active_workspace_id = workspace_id;
        self.next_workspace_id = next_workspace_id;
        Ok(OpenLocalProjectOutcome::Created {
            workspace_id,
            payload,
        })
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

    pub(crate) fn set_custom_name(
        &mut self,
        workspace_id: WorkspaceId,
        name: Option<String>,
    ) -> Result<(), WorkspaceError> {
        let Some(workspace) = self.workspace_mut(workspace_id) else {
            return Err(WorkspaceError::WorkspaceNotFound(workspace_id));
        };

        workspace.custom_name = name.filter(|name| !name.trim().is_empty());
        Ok(())
    }

    pub(crate) fn display_name(
        &self,
        workspace_id: WorkspaceId,
        home: &Path,
    ) -> Result<String, WorkspaceError> {
        let Some(target) = self.workspace(workspace_id) else {
            return Err(WorkspaceError::WorkspaceNotFound(workspace_id));
        };
        let Some(custom_name) = &target.custom_name else {
            let label = automatic_directory_label(&target.directory, home);
            if target.kind != WorkspaceKind::AdHoc {
                return Ok(label);
            }
            let occurrence = self
                .workspaces
                .iter()
                .take_while(|workspace| workspace.id != target.id)
                .filter(|workspace| {
                    workspace.kind == WorkspaceKind::AdHoc
                        && workspace.custom_name.is_none()
                        && workspace.directory == target.directory
                })
                .count()
                + 1;
            return Ok(if occurrence == 1 {
                label
            } else {
                format!("{label} {occurrence}")
            });
        };
        Ok(custom_name.clone())
    }

    pub(crate) fn kind(&self, workspace_id: WorkspaceId) -> Result<WorkspaceKind, WorkspaceError> {
        self.with_workspace(workspace_id, |workspace| workspace.kind)
    }

    pub(crate) fn workspace_directory(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<&Path, WorkspaceError> {
        self.with_workspace(workspace_id, |workspace| workspace.directory.as_path())
    }

    pub(crate) fn directory_available(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<bool, WorkspaceError> {
        self.with_workspace(workspace_id, |workspace| workspace.directory_available)
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the UI adopts Workspace kinds in a later migration"
        )
    )]
    pub(crate) fn project_root(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Option<&Path>, WorkspaceError> {
        self.with_workspace(workspace_id, |workspace| workspace.project_root.as_deref())
    }

    pub(crate) fn adopt_reported_directory(
        &mut self,
        workspace_id: WorkspaceId,
        candidate: &Path,
        validity: DirectoryValidity,
    ) -> Result<bool, WorkspaceError> {
        let Some(workspace) = self.workspace_mut(workspace_id) else {
            return Err(WorkspaceError::WorkspaceNotFound(workspace_id));
        };
        if workspace.kind != WorkspaceKind::AdHoc || validity == DirectoryValidity::Invalid {
            return Ok(false);
        }

        workspace.directory = candidate.to_path_buf();
        workspace.directory_available = true;
        Ok(true)
    }

    pub(crate) fn promote_authority_directory(
        &mut self,
        workspace_id: WorkspaceId,
        promoted: Option<(&Path, DirectoryValidity)>,
    ) -> Result<(), WorkspaceError> {
        let Some(workspace) = self.workspace_mut(workspace_id) else {
            return Err(WorkspaceError::WorkspaceNotFound(workspace_id));
        };
        if workspace.kind != WorkspaceKind::AdHoc {
            return Ok(());
        }
        if let Some((directory, DirectoryValidity::Valid)) = promoted {
            workspace.directory = directory.to_path_buf();
        }
        Ok(())
    }

    pub(crate) fn revalidate_directory(
        &mut self,
        workspace_id: WorkspaceId,
        validity: DirectoryValidity,
    ) -> Result<bool, WorkspaceError> {
        let Some(workspace) = self.workspace_mut(workspace_id) else {
            return Err(WorkspaceError::WorkspaceNotFound(workspace_id));
        };

        let available = validity == DirectoryValidity::Valid;
        let changed = workspace.directory_available != available;
        workspace.directory_available = available;
        Ok(changed)
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the UI adopts Workspace kinds in a later migration"
        )
    )]
    pub(crate) fn ensure_directory_available(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<(), WorkspaceError> {
        match self.workspace(workspace_id) {
            Some(workspace) if workspace.directory_available => Ok(()),
            Some(_) => Err(WorkspaceError::DirectoryUnavailable(workspace_id)),
            None => Err(WorkspaceError::WorkspaceNotFound(workspace_id)),
        }
    }

    pub(crate) fn close_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        replacement_launch: WorkspaceLaunch,
        create_replacement: impl FnOnce(WorkspaceId, &Path) -> T,
    ) -> Result<CloseWorkspaceOutcome<T>, WorkspaceError> {
        let Some(index) = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id == workspace_id)
        else {
            return Err(WorkspaceError::WorkspaceNotFound(workspace_id));
        };

        if self.workspaces.len() > 1 {
            let closed_workspace = self.workspaces.remove(index);
            if self.active_workspace_id == workspace_id {
                let fallback_index = index.min(self.workspaces.len() - 1);
                self.active_workspace_id = self.workspaces[fallback_index].id;
            }

            return Ok(CloseWorkspaceOutcome::WorkspaceClosed {
                closed_workspace_id: closed_workspace.id,
                active_workspace_id: self.active_workspace_id,
                payload: closed_workspace.payload,
            });
        }

        let (replacement_workspace_id, next_workspace_id) = self.next_workspace_id()?;
        let WorkspaceLaunch::AdHoc { home } = replacement_launch else {
            unreachable!("the final Workspace replacement is always an Ad Hoc Workspace");
        };
        let replacement_payload = create_replacement(replacement_workspace_id, &home);
        let closed_workspace = std::mem::replace(
            &mut self.workspaces[index],
            WorkspaceEntry {
                id: replacement_workspace_id,
                kind: WorkspaceKind::AdHoc,
                custom_name: None,
                project_root: None,
                directory: home,
                directory_available: true,
                payload: replacement_payload,
            },
        );
        self.active_workspace_id = replacement_workspace_id;
        self.next_workspace_id = next_workspace_id;

        Ok(CloseWorkspaceOutcome::FinalWorkspaceReplaced {
            closed_workspace_id: closed_workspace.id,
            replacement_workspace_id,
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

        Ok(FinalWindowCloseOutcome::WorkspaceClosed {
            closed_workspace_id: closed_workspace.id,
            active_workspace_id: self.active_workspace_id,
            payload: closed_workspace.payload,
        })
    }

    fn with_workspace<'a, R>(
        &'a self,
        workspace_id: WorkspaceId,
        read: impl FnOnce(&'a WorkspaceEntry<T>) -> R,
    ) -> Result<R, WorkspaceError> {
        match self.workspace(workspace_id) {
            Some(workspace) => Ok(read(workspace)),
            None => Err(WorkspaceError::WorkspaceNotFound(workspace_id)),
        }
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
}

fn automatic_directory_label(directory: &Path, home: &Path) -> String {
    if directory == home {
        return "Default".to_owned();
    }
    match directory.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => directory.to_string_lossy().into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    const HOME: &str = "/Users/test";
    const SITE_IDENTITY: CanonicalDirectoryId = CanonicalDirectoryId::new(7, 42);

    struct DropProbe {
        drops: Rc<Cell<usize>>,
    }

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.drops.update(|drops| drops + 1);
        }
    }

    fn home_path() -> PathBuf {
        PathBuf::from(HOME)
    }

    fn ad_hoc_launch() -> WorkspaceLaunch {
        WorkspaceLaunch::AdHoc { home: home_path() }
    }

    fn local_project_launch(project_root: &str) -> WorkspaceLaunch {
        WorkspaceLaunch::LocalProject {
            project_root: PathBuf::from(project_root),
        }
    }

    fn site_identity_of(path: &Path) -> Option<CanonicalDirectoryId> {
        (path == Path::new("/Users/test/Projects/Site")).then_some(SITE_IDENTITY)
    }

    fn new_workspaces<T>(payload: T) -> WorkspaceCollection<T> {
        WorkspaceCollection::new(home_path(), |_, working_directory| {
            assert_eq!(working_directory, Path::new(HOME));
            payload
        })
    }

    fn display_names(workspaces: &WorkspaceCollection<&str>) -> Vec<String> {
        workspaces
            .iter()
            .map(|workspace| {
                workspaces
                    .display_name(workspace.id(), Path::new(HOME))
                    .unwrap()
            })
            .collect()
    }

    #[test]
    fn new_should_create_one_active_ad_hoc_workspace_at_home() {
        let workspaces = new_workspaces("first payload");

        assert_eq!(
            (
                workspaces.len(),
                workspaces.active_workspace_id(),
                workspaces.active_workspace().id(),
                workspaces.active_workspace().payload(),
                workspaces.kind(WorkspaceId::new(1)).unwrap(),
                workspaces.workspace_directory(WorkspaceId::new(1)).unwrap(),
                workspaces.directory_available(WorkspaceId::new(1)).unwrap(),
                workspaces.project_root(WorkspaceId::new(1)).unwrap(),
                workspaces
                    .display_name(WorkspaceId::new(1), Path::new(HOME))
                    .unwrap()
                    .as_str(),
            ),
            (
                1,
                WorkspaceId::new(1),
                WorkspaceId::new(1),
                &"first payload",
                WorkspaceKind::AdHoc,
                Path::new(HOME),
                true,
                None,
                "Default",
            )
        );
    }

    #[test]
    fn iter_should_preserve_workspace_creation_order() {
        let mut workspaces = new_workspaces("first payload");
        workspaces
            .create_workspace(ad_hoc_launch(), |_, _| "second payload")
            .unwrap();
        workspaces
            .create_workspace(ad_hoc_launch(), |_, _| "third payload")
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
    fn create_ad_hoc_should_start_at_home_and_activate_regardless_of_other_directories() {
        let mut workspaces = new_workspaces("first payload");
        workspaces
            .adopt_reported_directory(
                WorkspaceId::new(1),
                Path::new("/elsewhere"),
                DirectoryValidity::Valid,
            )
            .unwrap();

        let created = workspaces
            .create_workspace(ad_hoc_launch(), |_, _| "second payload")
            .unwrap();

        assert_eq!(
            (
                created,
                workspaces.active_workspace_id(),
                workspaces.workspace_directory(created).unwrap(),
                workspaces.workspace_directory(WorkspaceId::new(1)).unwrap(),
            ),
            (
                WorkspaceId::new(2),
                WorkspaceId::new(2),
                Path::new(HOME),
                Path::new("/elsewhere"),
            )
        );
    }

    #[test]
    fn create_local_project_should_preserve_the_exact_selected_root() {
        let mut workspaces = new_workspaces("first payload");

        let created = workspaces
            .create_workspace(
                local_project_launch("/Users/test/Projects/Site"),
                |_, _| "project payload",
            )
            .unwrap();

        assert_eq!(
            (
                created,
                workspaces.active_workspace_id(),
                workspaces.kind(created).unwrap(),
                workspaces.workspace_directory(created).unwrap(),
                workspaces.project_root(created).unwrap(),
                workspaces.directory_available(created).unwrap(),
                workspaces
                    .display_name(created, Path::new(HOME))
                    .unwrap()
                    .as_str(),
            ),
            (
                WorkspaceId::new(2),
                WorkspaceId::new(2),
                WorkspaceKind::LocalProject,
                Path::new("/Users/test/Projects/Site"),
                Some(Path::new("/Users/test/Projects/Site")),
                true,
                "Site",
            )
        );
    }

    #[test]
    fn local_project_roots_should_ignore_reported_and_promoted_directories() {
        let mut workspaces = new_workspaces("first payload");
        let project = workspaces
            .create_workspace(
                local_project_launch("/Users/test/Projects/Site"),
                |_, _| "payload",
            )
            .unwrap();

        let adopted = workspaces.adopt_reported_directory(
            project,
            Path::new("/drifted"),
            DirectoryValidity::Valid,
        );
        workspaces
            .promote_authority_directory(
                project,
                Some((Path::new("/promoted"), DirectoryValidity::Valid)),
            )
            .unwrap();

        assert_eq!(
            (
                adopted,
                workspaces.workspace_directory(project).unwrap(),
                workspaces.project_root(project).unwrap(),
                workspaces.directory_available(project).unwrap(),
            ),
            (
                Ok(false),
                Path::new("/Users/test/Projects/Site"),
                Some(Path::new("/Users/test/Projects/Site")),
                true,
            )
        );
    }

    #[test]
    fn create_ad_hoc_factory_should_borrow_the_exact_stored_directory() {
        let mut workspaces = new_workspaces("first payload");
        let observed_pointer = Cell::new(std::ptr::null());

        workspaces
            .create_workspace(ad_hoc_launch(), |_, working_directory| {
                observed_pointer.set(working_directory.as_os_str().as_encoded_bytes().as_ptr());
                "second payload"
            })
            .unwrap();

        let stored_directory = workspaces.workspace_directory(WorkspaceId::new(2)).unwrap();
        assert_eq!(
            stored_directory,
            Path::new(HOME),
            "an Ad Hoc Workspace must start at HOME",
        );
        assert_eq!(
            observed_pointer.get(),
            stored_directory.as_os_str().as_encoded_bytes().as_ptr(),
            "payload construction must borrow the PathBuf stored by the Workspace",
        );
    }

    #[test]
    fn create_local_project_factory_should_borrow_the_exact_stored_directory() {
        let mut workspaces = new_workspaces("first payload");
        let observed_pointer = Cell::new(std::ptr::null());

        workspaces
            .create_workspace(
                local_project_launch("/Users/test/Projects/Site"),
                |_, working_directory| {
                    observed_pointer.set(working_directory.as_os_str().as_encoded_bytes().as_ptr());
                    "second payload"
                },
            )
            .unwrap();

        let stored_directory = workspaces.workspace_directory(WorkspaceId::new(2)).unwrap();
        assert_eq!(stored_directory, Path::new("/Users/test/Projects/Site"));
        assert_eq!(
            observed_pointer.get(),
            stored_directory.as_os_str().as_encoded_bytes().as_ptr(),
            "payload construction must borrow the PathBuf stored by the Workspace",
        );
    }

    #[test]
    fn create_workspace_should_reject_exhausted_ids_before_creating_its_payload() {
        let mut workspaces = new_workspaces("first payload");
        workspaces.next_workspace_id = u64::MAX;
        let creations = Cell::new(0);

        let result = workspaces.create_workspace(ad_hoc_launch(), |_, _| {
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
            .create_workspace(ad_hoc_launch(), |_, _| "second payload")
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
    fn set_custom_name_should_update_only_the_requested_workspace() {
        let mut workspaces = new_workspaces("first payload");
        workspaces
            .create_workspace(ad_hoc_launch(), |_, _| "second payload")
            .unwrap();

        workspaces
            .set_custom_name(WorkspaceId::new(1), Some("renamed".to_owned()))
            .unwrap();

        assert_eq!(display_names(&workspaces), vec!["renamed", "Default"]);
    }

    #[test]
    fn set_custom_name_should_reject_an_unknown_id_without_mutation() {
        let mut workspaces = new_workspaces("first payload");

        let result = workspaces.set_custom_name(WorkspaceId::new(99), Some("unknown".to_owned()));

        assert_eq!(
            (
                result,
                workspaces
                    .display_name(WorkspaceId::new(1), Path::new(HOME))
                    .unwrap()
                    .as_str(),
            ),
            (
                Err(WorkspaceError::WorkspaceNotFound(WorkspaceId::new(99))),
                "Default",
            )
        );
    }

    #[test]
    fn set_custom_name_should_restore_automatic_naming_when_cleared_or_blank() {
        let mut workspaces = new_workspaces("first payload");

        workspaces
            .set_custom_name(WorkspaceId::new(1), Some("Custom".to_owned()))
            .unwrap();
        assert_eq!(
            workspaces
                .display_name(WorkspaceId::new(1), Path::new(HOME))
                .unwrap(),
            "Custom"
        );
        workspaces
            .set_custom_name(WorkspaceId::new(1), None)
            .unwrap();
        assert_eq!(
            workspaces
                .display_name(WorkspaceId::new(1), Path::new(HOME))
                .unwrap(),
            "Default"
        );
        workspaces
            .set_custom_name(WorkspaceId::new(1), Some("   ".to_owned()))
            .unwrap();
        assert_eq!(
            workspaces
                .display_name(WorkspaceId::new(1), Path::new(HOME))
                .unwrap(),
            "Default"
        );
    }

    #[test]
    fn display_name_should_number_unrenamed_duplicates_in_sidebar_order() {
        let mut workspaces = new_workspaces("first payload");
        workspaces
            .create_workspace(ad_hoc_launch(), |_, _| "second payload")
            .unwrap();
        workspaces
            .create_workspace(ad_hoc_launch(), |_, _| "third payload")
            .unwrap();

        assert_eq!(
            display_names(&workspaces),
            vec!["Default", "Default 2", "Default 3"]
        );
    }

    #[test]
    fn renaming_one_duplicate_should_recalculate_the_remaining_numbers() {
        let mut workspaces = new_workspaces("first payload");
        workspaces
            .create_workspace(ad_hoc_launch(), |_, _| "second payload")
            .unwrap();
        workspaces
            .create_workspace(ad_hoc_launch(), |_, _| "third payload")
            .unwrap();

        workspaces
            .set_custom_name(WorkspaceId::new(1), Some("X".to_owned()))
            .unwrap();

        assert_eq!(
            display_names(&workspaces),
            vec!["X", "Default", "Default 2"]
        );
    }

    #[test]
    fn duplicate_custom_names_should_be_allowed() {
        let mut workspaces = new_workspaces("first payload");
        workspaces
            .create_workspace(ad_hoc_launch(), |_, _| "second payload")
            .unwrap();

        workspaces
            .set_custom_name(WorkspaceId::new(1), Some("Same".to_owned()))
            .unwrap();
        workspaces
            .set_custom_name(WorkspaceId::new(2), Some("Same".to_owned()))
            .unwrap();

        assert_eq!(display_names(&workspaces), vec!["Same", "Same"]);
    }

    #[test]
    fn custom_names_should_remain_stable_across_directory_changes() {
        let mut workspaces = new_workspaces("first payload");
        workspaces
            .create_workspace(ad_hoc_launch(), |_, _| "second payload")
            .unwrap();
        workspaces
            .set_custom_name(WorkspaceId::new(1), Some("X".to_owned()))
            .unwrap();

        workspaces
            .adopt_reported_directory(
                WorkspaceId::new(1),
                Path::new("/moved"),
                DirectoryValidity::Valid,
            )
            .unwrap();

        assert_eq!(display_names(&workspaces), vec!["X", "Default"]);
        assert_eq!(
            workspaces.workspace_directory(WorkspaceId::new(1)).unwrap(),
            Path::new("/moved")
        );
    }

    #[test]
    fn display_name_should_derive_the_current_directory_basename() {
        let mut workspaces = new_workspaces("first payload");

        workspaces
            .adopt_reported_directory(
                WorkspaceId::new(1),
                Path::new("/Users/test/Projects/SpaceTerm"),
                DirectoryValidity::Valid,
            )
            .unwrap();

        assert_eq!(
            workspaces
                .display_name(WorkspaceId::new(1), Path::new(HOME))
                .unwrap(),
            "SpaceTerm"
        );
    }

    #[test]
    fn local_project_display_names_should_not_join_ad_hoc_numbering() {
        let mut workspaces = new_workspaces("first payload");
        workspaces
            .adopt_reported_directory(
                WorkspaceId::new(1),
                Path::new("/proj/app"),
                DirectoryValidity::Valid,
            )
            .unwrap();
        workspaces
            .create_workspace(local_project_launch("/proj/app"), |_, _| "payload")
            .unwrap();
        workspaces
            .create_workspace(ad_hoc_launch(), |_, _| "payload")
            .unwrap();
        workspaces
            .adopt_reported_directory(
                WorkspaceId::new(3),
                Path::new("/proj/app"),
                DirectoryValidity::Valid,
            )
            .unwrap();

        assert_eq!(display_names(&workspaces), vec!["app", "app", "app 2"]);
    }

    #[test]
    fn open_local_project_should_create_and_activate_a_new_workspace() {
        let mut workspaces = new_workspaces("first payload");

        let outcome = workspaces
            .open_local_project(
                PathBuf::from("/Users/test/Projects/Site"),
                SITE_IDENTITY,
                site_identity_of,
                |_, _| "site payload",
            )
            .unwrap();

        let OpenLocalProjectOutcome::Created {
            workspace_id,
            payload,
        } = outcome
        else {
            panic!("opening an unseen Project must create a Workspace");
        };
        assert_eq!(
            (
                workspace_id,
                payload,
                workspaces.len(),
                workspaces.active_workspace_id(),
                workspaces.kind(workspace_id).unwrap(),
                workspaces.workspace_directory(workspace_id).unwrap(),
                workspaces.project_root(workspace_id).unwrap(),
                workspaces
                    .display_name(workspace_id, Path::new(HOME))
                    .unwrap()
                    .as_str(),
            ),
            (
                WorkspaceId::new(2),
                "site payload",
                2,
                WorkspaceId::new(2),
                WorkspaceKind::LocalProject,
                Path::new("/Users/test/Projects/Site"),
                Some(Path::new("/Users/test/Projects/Site")),
                "Site",
            )
        );
    }

    #[test]
    fn open_local_project_should_activate_the_existing_project_for_the_same_identity() {
        let mut workspaces = new_workspaces("first payload");
        let OpenLocalProjectOutcome::Created { workspace_id, .. } = workspaces
            .open_local_project(
                PathBuf::from("/Users/test/Projects/Site"),
                SITE_IDENTITY,
                site_identity_of,
                |_, _| "site payload",
            )
            .unwrap()
        else {
            panic!("the first opening must create a Workspace");
        };
        workspaces.activate_workspace(WorkspaceId::new(1)).unwrap();

        let outcome = workspaces
            .open_local_project(
                PathBuf::from("/private/Users/test/Projects/Site"),
                SITE_IDENTITY,
                site_identity_of,
                |_, _| unreachable!("an existing Project must not create a payload"),
            )
            .unwrap();

        assert_eq!(
            (
                outcome,
                workspaces.len(),
                workspaces.active_workspace_id(),
                workspaces.workspace_directory(workspace_id).unwrap(),
                workspaces.project_root(workspace_id).unwrap(),
            ),
            (
                OpenLocalProjectOutcome::ActivatedExisting {
                    existing_workspace_id: WorkspaceId::new(2),
                },
                2,
                WorkspaceId::new(2),
                Path::new("/Users/test/Projects/Site"),
                Some(Path::new("/Users/test/Projects/Site")),
            )
        );
    }

    #[test]
    fn open_local_project_should_create_when_an_ad_hoc_workspace_shares_the_path() {
        let mut workspaces = new_workspaces("first payload");
        workspaces
            .adopt_reported_directory(
                WorkspaceId::new(1),
                Path::new("/Users/test/Projects/Site"),
                DirectoryValidity::Valid,
            )
            .unwrap();

        let outcome = workspaces
            .open_local_project(
                PathBuf::from("/Users/test/Projects/Site"),
                SITE_IDENTITY,
                site_identity_of,
                |_, _| "site payload",
            )
            .unwrap();

        assert_eq!(
            (
                outcome,
                workspaces.len(),
                workspaces.active_workspace_id(),
                workspaces.kind(WorkspaceId::new(2)).unwrap(),
            ),
            (
                OpenLocalProjectOutcome::Created {
                    workspace_id: WorkspaceId::new(2),
                    payload: "site payload",
                },
                2,
                WorkspaceId::new(2),
                WorkspaceKind::LocalProject,
            )
        );
    }

    #[test]
    fn open_local_project_factory_should_borrow_the_exact_stored_directory() {
        let mut workspaces = new_workspaces("first payload");
        let observed_pointer = Cell::new(std::ptr::null());

        workspaces
            .open_local_project(
                PathBuf::from("/Users/test/Projects/Site"),
                SITE_IDENTITY,
                site_identity_of,
                |_, working_directory| {
                    observed_pointer.set(working_directory.as_os_str().as_encoded_bytes().as_ptr());
                    "site payload"
                },
            )
            .unwrap();

        let stored_directory = workspaces.workspace_directory(WorkspaceId::new(2)).unwrap();
        assert_eq!(
            observed_pointer.get(),
            stored_directory.as_os_str().as_encoded_bytes().as_ptr(),
            "payload construction must borrow the PathBuf stored by the Workspace",
        );
    }

    #[test]
    fn open_local_project_should_reject_exhausted_ids_before_creating_its_payload() {
        let mut workspaces = new_workspaces("first payload");
        workspaces.next_workspace_id = u64::MAX;
        let creations = Cell::new(0);

        let result = workspaces.open_local_project(
            PathBuf::from("/Users/test/Projects/Site"),
            SITE_IDENTITY,
            site_identity_of,
            |_, _| {
                creations.update(|count| count + 1);
                "site payload"
            },
        );

        assert_eq!(
            (result, creations.get(), workspaces.len()),
            (Err(WorkspaceError::IdSpaceExhausted), 0, 1,)
        );
    }

    #[test]
    fn open_local_project_should_activate_existing_without_allocating_an_id() {
        let mut workspaces = new_workspaces("first payload");
        workspaces
            .open_local_project(
                PathBuf::from("/Users/test/Projects/Site"),
                SITE_IDENTITY,
                site_identity_of,
                |_, _| "site payload",
            )
            .unwrap();
        workspaces.activate_workspace(WorkspaceId::new(1)).unwrap();
        workspaces.next_workspace_id = u64::MAX;

        let outcome = workspaces
            .open_local_project(
                PathBuf::from("/Users/test/Projects/Site"),
                SITE_IDENTITY,
                site_identity_of,
                |_, _| unreachable!("an existing Project must not create a payload"),
            )
            .unwrap();

        assert_eq!(
            (outcome, workspaces.active_workspace_id()),
            (
                OpenLocalProjectOutcome::ActivatedExisting {
                    existing_workspace_id: WorkspaceId::new(2),
                },
                WorkspaceId::new(2),
            )
        );
    }

    #[test]
    fn adopt_reported_directory_should_update_an_ad_hoc_directory_even_when_unavailable() {
        let mut workspaces = new_workspaces("first payload");
        workspaces
            .revalidate_directory(WorkspaceId::new(1), DirectoryValidity::Invalid)
            .unwrap();

        let adopted = workspaces
            .adopt_reported_directory(
                WorkspaceId::new(1),
                Path::new("/Users/test/Projects/SpaceTerm"),
                DirectoryValidity::Valid,
            )
            .unwrap();

        assert_eq!(
            (
                adopted,
                workspaces.workspace_directory(WorkspaceId::new(1)).unwrap(),
                workspaces.directory_available(WorkspaceId::new(1)).unwrap(),
            ),
            (true, Path::new("/Users/test/Projects/SpaceTerm"), true,)
        );
    }

    #[test]
    fn adopt_reported_directory_should_reject_invalid_reports_without_mutation() {
        let mut workspaces = new_workspaces("first payload");

        let adopted_while_available = workspaces
            .adopt_reported_directory(
                WorkspaceId::new(1),
                Path::new("/rejected"),
                DirectoryValidity::Invalid,
            )
            .unwrap();
        workspaces
            .revalidate_directory(WorkspaceId::new(1), DirectoryValidity::Invalid)
            .unwrap();
        let adopted_while_unavailable = workspaces
            .adopt_reported_directory(
                WorkspaceId::new(1),
                Path::new("/rejected"),
                DirectoryValidity::Invalid,
            )
            .unwrap();

        assert_eq!(
            (
                adopted_while_available,
                adopted_while_unavailable,
                workspaces.workspace_directory(WorkspaceId::new(1)).unwrap(),
                workspaces.directory_available(WorkspaceId::new(1)).unwrap(),
            ),
            (false, false, Path::new(HOME), false)
        );
    }

    #[test]
    fn promote_authority_directory_should_adopt_valid_reports_and_retain_previous_otherwise() {
        let mut workspaces = new_workspaces("first payload");

        workspaces
            .promote_authority_directory(
                WorkspaceId::new(1),
                Some((Path::new("/promoted"), DirectoryValidity::Valid)),
            )
            .unwrap();
        let after_valid = workspaces
            .workspace_directory(WorkspaceId::new(1))
            .unwrap()
            .to_path_buf();
        workspaces
            .promote_authority_directory(
                WorkspaceId::new(1),
                Some((Path::new("/invalid"), DirectoryValidity::Invalid)),
            )
            .unwrap();
        let after_invalid = workspaces
            .workspace_directory(WorkspaceId::new(1))
            .unwrap()
            .to_path_buf();
        workspaces
            .promote_authority_directory(WorkspaceId::new(1), None)
            .unwrap();
        let after_none = workspaces
            .workspace_directory(WorkspaceId::new(1))
            .unwrap()
            .to_path_buf();
        workspaces
            .revalidate_directory(WorkspaceId::new(1), DirectoryValidity::Invalid)
            .unwrap();
        workspaces
            .promote_authority_directory(
                WorkspaceId::new(1),
                Some((Path::new("/later"), DirectoryValidity::Valid)),
            )
            .unwrap();

        assert_eq!(
            (
                after_valid,
                after_invalid,
                after_none,
                workspaces
                    .workspace_directory(WorkspaceId::new(1))
                    .unwrap()
                    .to_path_buf(),
                workspaces.directory_available(WorkspaceId::new(1)).unwrap(),
            ),
            (
                PathBuf::from("/promoted"),
                PathBuf::from("/promoted"),
                PathBuf::from("/promoted"),
                PathBuf::from("/later"),
                false,
            )
        );
    }

    #[test]
    fn revalidate_directory_should_flip_availability_and_report_changes() {
        let mut workspaces = new_workspaces("first payload");

        let invalidated = workspaces
            .revalidate_directory(WorkspaceId::new(1), DirectoryValidity::Invalid)
            .unwrap();
        let stayed_invalid = workspaces
            .revalidate_directory(WorkspaceId::new(1), DirectoryValidity::Invalid)
            .unwrap();
        let validated = workspaces
            .revalidate_directory(WorkspaceId::new(1), DirectoryValidity::Valid)
            .unwrap();
        let project = workspaces
            .create_workspace(
                local_project_launch("/Users/test/Projects/Site"),
                |_, _| "payload",
            )
            .unwrap();
        let project_invalidated = workspaces
            .revalidate_directory(project, DirectoryValidity::Invalid)
            .unwrap();

        assert_eq!(
            (
                invalidated,
                stayed_invalid,
                validated,
                workspaces.directory_available(WorkspaceId::new(1)).unwrap(),
                project_invalidated,
                workspaces.directory_available(project).unwrap(),
            ),
            (true, false, true, true, true, false)
        );
    }

    #[test]
    fn ensure_directory_available_should_error_while_unavailable() {
        let mut workspaces = new_workspaces("first payload");
        workspaces
            .revalidate_directory(WorkspaceId::new(1), DirectoryValidity::Invalid)
            .unwrap();

        let unavailable = workspaces.ensure_directory_available(WorkspaceId::new(1));
        workspaces
            .revalidate_directory(WorkspaceId::new(1), DirectoryValidity::Valid)
            .unwrap();
        let available = workspaces.ensure_directory_available(WorkspaceId::new(1));
        let unknown = workspaces.ensure_directory_available(WorkspaceId::new(99));

        assert_eq!(
            (unavailable, available, unknown),
            (
                Err(WorkspaceError::DirectoryUnavailable(WorkspaceId::new(1))),
                Ok(()),
                Err(WorkspaceError::WorkspaceNotFound(WorkspaceId::new(99))),
            )
        );
    }

    #[test]
    fn non_final_close_should_not_allocate_a_replacement_workspace_id() {
        let mut workspaces = new_workspaces("first payload");
        workspaces
            .create_workspace(ad_hoc_launch(), |_, _| "second payload")
            .unwrap();
        workspaces.next_workspace_id = u64::MAX;

        let outcome = workspaces
            .close_workspace(WorkspaceId::new(1), ad_hoc_launch(), |_, _| {
                unreachable!("a replacement is not needed")
            })
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
            .create_workspace(ad_hoc_launch(), |_, _| "second payload")
            .unwrap();
        workspaces
            .close_workspace(second, ad_hoc_launch(), |_, _| {
                unreachable!("a replacement is not needed")
            })
            .unwrap();

        let third = workspaces
            .create_workspace(ad_hoc_launch(), |_, _| "third payload")
            .unwrap();

        assert_eq!(third, WorkspaceId::new(3));
    }

    #[test]
    fn close_workspace_should_focus_the_next_workspace_when_closing_the_active_middle_workspace() {
        let mut workspaces = new_workspaces("first payload");
        workspaces
            .create_workspace(ad_hoc_launch(), |_, _| "second payload")
            .unwrap();
        workspaces
            .create_workspace(ad_hoc_launch(), |_, _| "third payload")
            .unwrap();
        workspaces.activate_workspace(WorkspaceId::new(2)).unwrap();

        let outcome = workspaces
            .close_workspace(WorkspaceId::new(2), ad_hoc_launch(), |_, _| {
                unreachable!("a replacement is not needed")
            })
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
            .create_workspace(ad_hoc_launch(), |_, _| "second payload")
            .unwrap();

        let outcome = workspaces
            .close_workspace(WorkspaceId::new(2), ad_hoc_launch(), |_, _| {
                unreachable!("a replacement is not needed")
            })
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

        let result = workspaces.close_workspace(WorkspaceId::new(99), ad_hoc_launch(), |_, _| {
            creations.update(|count| count + 1);
            "replacement payload"
        });

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
    fn close_workspace_should_replace_the_final_workspace_with_an_ad_hoc_workspace_at_home() {
        let mut workspaces = new_workspaces("first payload");
        workspaces
            .adopt_reported_directory(
                WorkspaceId::new(1),
                Path::new("/Users/test/Projects/Drifted"),
                DirectoryValidity::Valid,
            )
            .unwrap();
        workspaces
            .set_custom_name(WorkspaceId::new(1), Some("Drifted".to_owned()))
            .unwrap();
        let observed_pointer = Cell::new(std::ptr::null());

        let outcome = workspaces
            .close_workspace(
                WorkspaceId::new(1),
                ad_hoc_launch(),
                |_, working_directory| {
                    observed_pointer.set(working_directory.as_os_str().as_encoded_bytes().as_ptr());
                    "replacement payload"
                },
            )
            .unwrap();

        let replacement_id = WorkspaceId::new(2);
        let stored_directory = workspaces.workspace_directory(replacement_id).unwrap();
        assert_eq!(
            (
                outcome,
                workspaces.len(),
                workspaces.active_workspace_id(),
                workspaces.kind(replacement_id).unwrap(),
                stored_directory,
                workspaces.directory_available(replacement_id).unwrap(),
                workspaces.project_root(replacement_id).unwrap(),
                workspaces
                    .display_name(replacement_id, Path::new(HOME))
                    .unwrap()
                    .as_str(),
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
                WorkspaceKind::AdHoc,
                Path::new(HOME),
                true,
                None,
                "Default",
                &"replacement payload",
            )
        );
        assert_eq!(
            observed_pointer.get(),
            stored_directory.as_os_str().as_encoded_bytes().as_ptr(),
            "replacement construction must borrow the PathBuf stored by the Workspace",
        );
    }

    #[test]
    fn close_workspace_should_reject_exhausted_ids_before_factory_side_effects() {
        let mut workspaces = new_workspaces("first payload");
        workspaces.next_workspace_id = u64::MAX;
        let creations = Cell::new(0);

        let result = workspaces.close_workspace(WorkspaceId::new(1), ad_hoc_launch(), |_, _| {
            creations.update(|count| count + 1);
            "replacement payload"
        });

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
            .close_workspace(WorkspaceId::new(1), ad_hoc_launch(), |_, _| DropProbe {
                drops: Rc::clone(&drops),
            })
            .unwrap();
        assert_eq!(drops.get(), 0);

        drop(outcome);
        assert_eq!(drops.get(), 1);

        drop(workspaces);
        assert_eq!(drops.get(), 2);
    }

    #[test]
    fn final_window_close_should_remove_a_non_final_workspace_without_allocating_a_replacement() {
        let mut workspaces = new_workspaces("first payload");
        workspaces
            .create_workspace(ad_hoc_launch(), |_, _| "second payload")
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
            .create_workspace(ad_hoc_launch(), |_, _| DropProbe {
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
}
