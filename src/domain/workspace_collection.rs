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
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the Workspace root is owned here for future root-scoped operations"
        )
    )]
    working_directory: PathBuf,
    payload: T,
}

impl<T> WorkspaceEntry<T> {
    pub(crate) const fn id(&self) -> WorkspaceId {
        self.id
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    #[cfg(test)]
    pub(crate) fn working_directory(&self) -> &Path {
        &self.working_directory
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
        working_directory: PathBuf,
        create_initial_payload: impl FnOnce(WorkspaceId, &Path) -> T,
    ) -> Self {
        let initial_workspace_id = WorkspaceId::from_raw(1);
        let payload = create_initial_payload(initial_workspace_id, &working_directory);
        Self {
            workspaces: vec![WorkspaceEntry {
                id: initial_workspace_id,
                name: default_workspace_name(1),
                working_directory,
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
        working_directory: PathBuf,
        create_payload: impl FnOnce(WorkspaceId, &Path) -> T,
    ) -> Result<WorkspaceId, WorkspaceError> {
        let (workspace_id, next_workspace_id) = self.next_workspace_id()?;
        let name = self.next_default_workspace_name(None);
        let payload = create_payload(workspace_id, &working_directory);
        self.workspaces.push(WorkspaceEntry {
            id: workspace_id,
            name,
            working_directory,
            payload,
        });
        self.active_workspace_id = workspace_id;
        self.next_workspace_id = next_workspace_id;
        Ok(workspace_id)
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
        let Some(workspace) = self.workspace_mut(workspace_id) else {
            return Err(WorkspaceError::WorkspaceNotFound(workspace_id));
        };

        workspace.name = name;
        Ok(())
    }

    pub(crate) fn close_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        replacement_working_directory: PathBuf,
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
            let replacement_name = self.next_default_workspace_name(Some(workspace_id));
            let replacement_payload =
                create_replacement(replacement_workspace_id, &replacement_working_directory);
            let closed_workspace = std::mem::replace(
                &mut self.workspaces[index],
                WorkspaceEntry {
                    id: replacement_workspace_id,
                    name: replacement_name,
                    working_directory: replacement_working_directory,
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
