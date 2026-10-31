mod naming;

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
pub enum CurrentDirectory {
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
pub enum RemoteWorkspaceValueError {
    #[error("SSH destination must be one non-option, control-free token")]
    Destination,
    #[error("Remote Directory must be an absolute or ~/ path without control characters")]
    StartingDirectory,
}

/// The exact OpenSSH destination token selected by the user.
///
/// Equality is spelling-sensitive so different configured aliases remain distinct even when they
/// currently resolve to the same host. The token is passed as one validated OpenSSH argument.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SshDestination(String);

impl fmt::Debug for SshDestination {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SshDestination(<redacted>)")
    }
}

impl SshDestination {
    pub fn new(value: String) -> Result<Self, RemoteWorkspaceValueError> {
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

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The exact user-visible spelling of a directory selected on a remote destination.
///
/// This remote value is preserved for display and shell startup. It is not local filesystem
/// authority and must never be converted to `PathBuf` or passed to a local path API.
#[derive(Clone, Eq, PartialEq)]
pub struct RemoteDirectory(String);

impl fmt::Debug for RemoteDirectory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RemoteDirectory(<redacted>)")
    }
}

impl RemoteDirectory {
    pub fn home() -> Self {
        Self("~".into())
    }

    pub fn new(value: String) -> Result<Self, RemoteWorkspaceValueError> {
        let supported_form = value.starts_with('/') || value == "~" || value.starts_with("~/");
        if !supported_form || value.chars().any(char::is_control) {
            return Err(RemoteWorkspaceValueError::StartingDirectory);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Eq)]
/// A validated local directory and its local filesystem identity.
///
/// Local launch and pin operations retain this authority when passing directories to native APIs.
pub struct ValidatedLocalDirectory {
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
    pub fn new(path: PathBuf, identity: LocalDirectoryIdentity) -> Self {
        Self { path, identity }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn identity(&self) -> LocalDirectoryIdentity {
        self.identity.clone()
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
    #[error("Workspace position {position} is outside a collection of {len} Workspaces")]
    PositionOutOfRange { position: usize, len: usize },
}

pub(crate) struct WorkspaceEntry<T> {
    id: WorkspaceId,
    name: String,
    custom_name: Option<String>,
    automatic_name_base: String,
    automatic_directory: Option<PathBuf>,
    /// An explicit starting-directory override with its local authority.
    pinned_directory: Option<ValidatedLocalDirectory>,
    home_directory: ValidatedLocalDirectory,
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
        Some(self.home_directory.path())
    }

    pub(crate) fn local_display_directory(&self) -> Option<&Path> {
        match &self.pinned_directory {
            Some(directory) => Some(directory.path()),
            None => match &self.automatic_directory {
                Some(directory) => Some(directory),
                None => self.local_home_directory(),
            },
        }
    }

    pub(crate) const fn pinned_directory(&self) -> Option<&ValidatedLocalDirectory> {
        self.pinned_directory.as_ref()
    }

    pub(crate) const fn payload(&self) -> &T {
        &self.payload
    }
}

pub(crate) struct WorkspaceCollection<T> {
    workspaces: Vec<WorkspaceEntry<T>>,
    parked: Vec<(usize, WorkspaceEntry<T>)>,
    /// The Workspace the window presents. It is `None` only while the collection is empty.
    active_workspace_id: Option<WorkspaceId>,
    next_workspace_id: u64,
}

impl<T> WorkspaceCollection<T> {
    /// A collection presenting nothing yet, whose Workspaces another owner supplies.
    pub(crate) const fn empty() -> Self {
        Self {
            workspaces: Vec::new(),
            parked: Vec::new(),
            active_workspace_id: None,
            next_workspace_id: 1,
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.workspaces.len()
    }

    /// The Active Workspace, absent only while the collection is empty.
    pub(crate) const fn active_workspace_id(&self) -> Option<WorkspaceId> {
        self.active_workspace_id
    }

    /// The Active Workspace, absent only while the collection is empty.
    pub(crate) fn active_workspace(&self) -> Option<&WorkspaceEntry<T>> {
        self.workspace(self.active_workspace_id?)
    }

    pub(crate) fn workspace(&self, workspace_id: WorkspaceId) -> Option<&WorkspaceEntry<T>> {
        self.workspaces
            .iter()
            .find(|workspace| workspace.id == workspace_id)
    }

    pub(crate) fn iter(&self) -> impl ExactSizeIterator<Item = &WorkspaceEntry<T>> {
        self.workspaces.iter()
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
            automatic_name_base: "Default".to_owned(),
            automatic_directory: None,
            name,
            custom_name: None,
            pinned_directory: None,
            home_directory: directory,
            payload,
        });
        self.active_workspace_id = Some(id);
        self.next_workspace_id = next;
        self.recalculate_automatic_names();
        Ok(id)
    }

    pub(crate) fn update_automatic_directory(
        &mut self,
        workspace_id: WorkspaceId,
        directory: PathBuf,
    ) -> Result<(), WorkspaceError> {
        let workspace = self
            .workspace_mut(workspace_id)
            .ok_or(WorkspaceError::WorkspaceNotFound(workspace_id))?;
        if workspace.automatic_directory.as_ref() == Some(&directory) {
            return Ok(());
        }
        workspace.automatic_directory = Some(directory);
        self.recalculate_automatic_names();
        Ok(())
    }

    pub(crate) fn set_pinned_directory(
        &mut self,
        workspace_id: WorkspaceId,
        pin: Option<ValidatedLocalDirectory>,
    ) -> Result<(), WorkspaceError> {
        let workspace = self
            .workspace_mut(workspace_id)
            .ok_or(WorkspaceError::WorkspaceNotFound(workspace_id))?;
        workspace.pinned_directory = pin;
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

        self.active_workspace_id = Some(workspace_id);
        Ok(())
    }

    /// Moves a Workspace to `position` in the Workspace order and reports whether the order
    /// changed. Only presentation order changes.
    pub(crate) fn move_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        position: usize,
    ) -> Result<bool, WorkspaceError> {
        let index = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id == workspace_id)
            .ok_or(WorkspaceError::WorkspaceNotFound(workspace_id))?;
        if position >= self.workspaces.len() {
            return Err(WorkspaceError::PositionOutOfRange {
                position,
                len: self.workspaces.len(),
            });
        }
        if index == position {
            return Ok(false);
        }

        let workspace = self.workspaces.remove(index);
        self.workspaces.insert(position, workspace);
        Ok(true)
    }

    /// A supplied name is explicit even when it matches an automatic name.
    pub(crate) fn rename_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        name: String,
    ) -> Result<(), WorkspaceError> {
        let workspace = self
            .workspace_mut(workspace_id)
            .ok_or(WorkspaceError::WorkspaceNotFound(workspace_id))?;
        if name.trim().is_empty() {
            return Ok(());
        }
        workspace.custom_name = Some(name.trim().to_owned());
        self.recalculate_automatic_names();
        Ok(())
    }

    /// Removes a Workspace from the presentation without replacing it, returning its payload.
    ///
    /// Unlike closing, removing the final Workspace leaves the collection empty, with no Active
    /// Workspace, until a later creation presents one. A removed Active Workspace passes
    /// activation to the Workspace that takes its place in the order.
    #[cfg_attr(
        all(not(feature = "desktop"), not(test)),
        expect(
            dead_code,
            reason = "only Detach Workspace removes without a replacement"
        )
    )]
    pub(crate) fn remove_workspace(
        &mut self,
        workspace_id: WorkspaceId,
    ) -> Result<T, WorkspaceError> {
        let (position, entry) = self.take_workspace(workspace_id)?;
        self.retire_parked_position(self.position_with_parked(position));
        Ok(entry.payload)
    }

    /// Removes a presentation while keeping its identity, payload and order for a refused close.
    pub(crate) fn park_workspace(
        &mut self,
        workspace_id: WorkspaceId,
    ) -> Result<(), WorkspaceError> {
        let (position, entry) = self.take_workspace(workspace_id)?;
        let position = self.position_with_parked(position);
        let index = self.parked.partition_point(|(other, _)| *other < position);
        self.parked.insert(index, (position, entry));
        Ok(())
    }

    pub(crate) fn parked_ids(&self) -> impl Iterator<Item = WorkspaceId> + '_ {
        self.parked.iter().map(|(_, entry)| entry.id)
    }

    /// Restores a refused close without replacing a newer Active Workspace.
    pub(crate) fn restore_workspace(
        &mut self,
        workspace_id: WorkspaceId,
    ) -> Result<(), WorkspaceError> {
        let index = self
            .parked
            .iter()
            .position(|(_, entry)| entry.id == workspace_id)
            .ok_or(WorkspaceError::WorkspaceNotFound(workspace_id))?;
        let (position, entry) = self.parked.remove(index);
        self.workspaces.insert(
            position.saturating_sub(index).min(self.workspaces.len()),
            entry,
        );
        if self.active_workspace_id.is_none() {
            self.active_workspace_id = Some(workspace_id);
        }
        self.recalculate_automatic_names();
        Ok(())
    }

    /// Ends a parked presentation only after its removal is authoritative.
    pub(crate) fn discard_parked_workspace(
        &mut self,
        workspace_id: WorkspaceId,
    ) -> Result<T, WorkspaceError> {
        let index = self
            .parked
            .iter()
            .position(|(_, entry)| entry.id == workspace_id)
            .ok_or(WorkspaceError::WorkspaceNotFound(workspace_id))?;
        let (position, entry) = self.parked.remove(index);
        self.retire_parked_position(position);
        Ok(entry.payload)
    }

    fn position_with_parked(&self, mut visible_position: usize) -> usize {
        for (position, _) in &self.parked {
            if *position <= visible_position {
                visible_position += 1;
            }
        }
        visible_position
    }

    fn retire_parked_position(&mut self, position: usize) {
        for (other, _) in &mut self.parked {
            if *other > position {
                *other -= 1;
            }
        }
    }

    fn take_workspace(
        &mut self,
        workspace_id: WorkspaceId,
    ) -> Result<(usize, WorkspaceEntry<T>), WorkspaceError> {
        let index = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id == workspace_id)
            .ok_or(WorkspaceError::WorkspaceNotFound(workspace_id))?;
        let removed = self.workspaces.remove(index);
        if self.active_workspace_id == Some(workspace_id) {
            self.active_workspace_id = self
                .workspaces
                .get(index.min(self.workspaces.len().saturating_sub(1)))
                .map(WorkspaceEntry::id);
        }
        self.recalculate_automatic_names();
        Ok((index, removed))
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
    if workspace_number == 1 {
        "Default".to_owned()
    } else {
        format!("Default ({workspace_number})")
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn refused_closes_restore_identity_and_order_without_replacing_newer_activation() {
        for reverse in [false, true] {
            let mut workspaces = new_workspaces("first");
            let first = workspaces.active_workspace_id().unwrap();
            let second = workspaces
                .create_local_workspace(validated("/second", 1), |_, _| "second")
                .unwrap();
            workspaces.park_workspace(first).unwrap();
            workspaces.park_workspace(second).unwrap();
            let later = workspaces
                .create_local_workspace(validated("/later", 2), |_, _| "later")
                .unwrap();
            for id in if reverse {
                [second, first]
            } else {
                [first, second]
            } {
                workspaces.restore_workspace(id).unwrap();
            }
            assert_eq!(
                workspaces
                    .iter()
                    .map(WorkspaceEntry::id)
                    .collect::<Vec<_>>(),
                [first, second, later]
            );
            assert_eq!(workspaces.workspace(first).unwrap().payload(), &"first");
            assert_eq!(workspaces.active_workspace_id(), Some(later));
        }
    }

    /// Reads the Active Workspace of a collection a test knows is not empty.
    pub(crate) trait PresentedWorkspace<T> {
        fn active(&self) -> &WorkspaceEntry<T>;

        fn active_id(&self) -> WorkspaceId {
            self.active().id()
        }
    }

    impl<T> PresentedWorkspace<T> for WorkspaceCollection<T> {
        fn active(&self) -> &WorkspaceEntry<T> {
            self.active_workspace()
                .expect("the collection should present an Active Workspace")
        }
    }

    fn new_workspaces<T>(payload: T) -> WorkspaceCollection<T> {
        local_collection(validated("/first", 0), |_, working_directory| {
            assert_eq!(working_directory, Path::new("/first"));
            payload
        })
    }

    /// A collection presenting one Workspace at `directory`, as a window's first one.
    pub(crate) fn local_collection<T>(
        directory: ValidatedLocalDirectory,
        payload: impl FnOnce(WorkspaceId, &Path) -> T,
    ) -> WorkspaceCollection<T> {
        let mut workspaces = WorkspaceCollection::empty();
        workspaces
            .create_local_workspace(directory, payload)
            .expect("an empty collection accepts its first Workspace");
        workspaces
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
    fn remote_values_validate_without_changing_user_visible_spelling() {
        let destination = ssh_destination("root@fedora@orb");
        let directory = remote_directory("~/Projects/Space Term/");

        assert_eq!(
            (destination.as_str(), directory.as_str()),
            ("root@fedora@orb", "~/Projects/Space Term/")
        );
        assert!(SshDestination::new("bad destination".to_owned()).is_err());
        assert!(RemoteDirectory::new("relative/path".to_owned()).is_err());
    }

    #[test]
    fn pins_are_explicit_independent_and_do_not_change_home() {
        let mut workspaces = local_collection(validated("/home/me", 1), |_, _| ());
        let first = workspaces.active_id();
        let second = workspaces
            .create_local_workspace(validated("/home/me", 1), |_, _| ())
            .unwrap();
        let pin = validated("/app/frontend", 2);
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
    fn new_should_create_one_valid_active_workspace() {
        let workspaces = new_workspaces("first payload");

        assert_eq!(
            (
                workspaces.len(),
                workspaces.active_id(),
                workspaces.active().id(),
                workspaces.active().payload(),
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
            .create_local_workspace(validated("/second", 0), |_, _| "second payload")
            .unwrap();
        workspaces
            .create_local_workspace(validated("/third", 0), |_, _| "third payload")
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
    fn create_local_workspace_should_create_and_activate_the_new_workspace() {
        let mut workspaces = new_workspaces("first payload");

        let created = workspaces
            .create_local_workspace(validated("/second", 0), |_, _| "second payload")
            .unwrap();

        assert_eq!(
            (
                created,
                workspaces.len(),
                workspaces.active_id(),
                workspaces.active().payload(),
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
    fn create_local_workspace_should_choose_the_first_available_default_name() {
        let mut workspaces = new_workspaces("first payload");
        workspaces
            .create_local_workspace(validated("/second", 0), |_, _| "second payload")
            .unwrap();
        workspaces
            .rename_workspace(WorkspaceId::new(1), "Projects".to_owned())
            .unwrap();

        workspaces
            .create_local_workspace(validated("/third", 0), |_, _| "third payload")
            .unwrap();

        let names = workspaces
            .iter()
            .map(WorkspaceEntry::name)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["Projects", "Default (2)", "Default"]);
    }

    #[test]
    fn create_local_workspace_should_assign_its_name_and_propagate_the_exact_working_directory() {
        let mut workspaces = new_workspaces("first payload");
        let working_directory = PathBuf::from("/Users/test/projects");

        workspaces
            .create_local_workspace(
                ValidatedLocalDirectory::new(
                    working_directory,
                    LocalDirectoryIdentity::for_test(0),
                ),
                |id, payload_working_directory| {
                    assert_eq!(id, WorkspaceId::new(2));
                    assert_eq!(payload_working_directory, Path::new("/Users/test/projects"));
                    "second payload"
                },
            )
            .unwrap();

        let stored_working_directory = workspaces.active().local_home_directory().unwrap();
        assert_eq!(
            (workspaces.active().name(), stored_working_directory,),
            ("Default (2)", Path::new("/Users/test/projects"))
        );
    }

    #[test]
    fn create_local_workspace_should_reject_exhausted_ids_before_creating_its_payload() {
        let mut workspaces = new_workspaces("first payload");
        workspaces.next_workspace_id = u64::MAX;
        let creations = Cell::new(0);

        let result = workspaces.create_local_workspace(validated("/second", 0), |_, _| {
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
            .create_local_workspace(validated("/second", 0), |_, _| "second payload")
            .unwrap();

        workspaces.activate_workspace(WorkspaceId::new(1)).unwrap();

        assert_eq!(
            (workspaces.active_id(), workspaces.active().payload(),),
            (WorkspaceId::new(1), &"first payload")
        );
    }

    #[test]
    fn move_workspace_should_reorder_without_changing_the_active_workspace_or_names() {
        let mut workspaces = new_workspaces("first");
        workspaces
            .create_local_workspace(validated("/second", 0), |_, _| "second")
            .unwrap();
        let third = workspaces
            .create_local_workspace(validated("/second", 0), |_, _| "third")
            .unwrap();
        let names_before = workspaces
            .iter()
            .map(|workspace| (workspace.id(), workspace.name().to_owned()))
            .collect::<std::collections::BTreeMap<_, _>>();

        let moved = workspaces.move_workspace(third, 0).unwrap();
        let unchanged = workspaces.move_workspace(third, 0).unwrap();

        assert_eq!(
            (
                moved,
                unchanged,
                workspaces
                    .iter()
                    .map(|workspace| *workspace.payload())
                    .collect::<Vec<_>>(),
                workspaces.active_id(),
                workspaces
                    .iter()
                    .map(|workspace| (workspace.id(), workspace.name().to_owned()))
                    .collect::<std::collections::BTreeMap<_, _>>(),
            ),
            (
                true,
                false,
                vec!["third", "first", "second"],
                third,
                names_before,
            )
        );
    }

    #[test]
    fn move_workspace_should_reject_an_unknown_workspace_or_position_without_mutation() {
        let mut workspaces = new_workspaces("first");
        workspaces
            .create_local_workspace(validated("/second", 0), |_, _| "second")
            .unwrap();

        let unknown = workspaces.move_workspace(WorkspaceId::new(99), 0);
        let beyond = workspaces.move_workspace(WorkspaceId::new(1), 2);

        assert_eq!(
            (
                unknown,
                beyond,
                workspaces
                    .iter()
                    .map(|workspace| *workspace.payload())
                    .collect::<Vec<_>>(),
            ),
            (
                Err(WorkspaceError::WorkspaceNotFound(WorkspaceId::new(99))),
                Err(WorkspaceError::PositionOutOfRange {
                    position: 2,
                    len: 2
                }),
                vec!["first", "second"],
            )
        );
    }

    #[test]
    fn activate_workspace_should_reject_an_unknown_id_without_mutation() {
        let mut workspaces = new_workspaces("first payload");

        let result = workspaces.activate_workspace(WorkspaceId::new(99));

        assert_eq!(
            (result, workspaces.active_id()),
            (
                Err(WorkspaceError::WorkspaceNotFound(WorkspaceId::new(99))),
                WorkspaceId::new(1),
            )
        );
    }

    #[test]
    fn creation_name_should_preserve_explicit_names_including_duplicates() {
        let mut workspaces = new_workspaces(());
        let first = workspaces.active_id();
        workspaces
            .rename_workspace(first, "Project".to_owned())
            .unwrap();
        let second = workspaces
            .create_local_workspace(validated("/second", 0), |_, _| ())
            .unwrap();
        workspaces
            .rename_workspace(second, "Project 2".to_owned())
            .unwrap();
        for directory in ["/third", "/fourth"] {
            let id = workspaces
                .create_local_workspace(validated(directory, 0), |_, _| ())
                .unwrap();
            workspaces
                .rename_workspace(id, " Project ".to_owned())
                .unwrap();
        }

        assert_eq!(
            workspaces
                .iter()
                .map(WorkspaceEntry::name)
                .collect::<Vec<_>>(),
            ["Project", "Project 2", "Project", "Project"]
        );
    }

    #[test]
    fn creation_name_should_claim_an_automatic_name_and_remain_frozen_after_directory_changes() {
        let mut workspaces = new_workspaces(());
        let automatic_name = workspaces.active().name().to_owned();
        let first = workspaces.active_id();
        workspaces
            .rename_workspace(first, automatic_name.clone())
            .unwrap();
        assert_eq!(
            workspaces.active().custom_name.as_deref(),
            Some(automatic_name.as_str())
        );
        workspaces
            .set_pinned_directory(
                first,
                Some(validated("/different/service", 91)),
            )
            .unwrap();
        assert_eq!(workspaces.workspace(first).unwrap().name(), automatic_name);
        let id = workspaces
            .create_local_workspace(validated("/second", 0), |_, _| ())
            .unwrap();
        workspaces
            .rename_workspace(id, automatic_name.clone())
            .unwrap();
        workspaces
            .set_pinned_directory(
                id,
                Some(validated("/different/project", 90)),
            )
            .unwrap();

        let workspace = workspaces.workspace(id).unwrap();
        let expected = automatic_name;
        assert_eq!(
            (workspace.name(), workspace.custom_name.as_deref()),
            (expected.as_str(), Some(expected.as_str()))
        );
    }

    #[test]
    fn creation_name_should_keep_blank_names_automatic() {
        let mut workspaces = new_workspaces(());
        let id = workspaces.active_id();
        workspaces.rename_workspace(id, " \t ".to_owned()).unwrap();

        assert_eq!(workspaces.active().custom_name, None);
    }

    #[test]
    fn creation_name_should_preserve_case_sensitive_matching_and_manual_rename_semantics() {
        let mut workspaces = new_workspaces(());
        let first = workspaces.active_id();
        workspaces
            .rename_workspace(first, "Project".to_owned())
            .unwrap();
        let second = workspaces
            .create_local_workspace(validated("/second", 0), |_, _| ())
            .unwrap();
        workspaces
            .rename_workspace(second, "project".to_owned())
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
            .create_local_workspace(validated("/second", 0), |_, _| "second payload")
            .unwrap();

        workspaces
            .rename_workspace(WorkspaceId::new(1), "renamed".to_owned())
            .unwrap();

        let names = workspaces
            .iter()
            .map(WorkspaceEntry::name)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["renamed", "Default (2)"]);
    }

    #[test]
    fn rename_workspace_should_reject_an_unknown_id_without_mutation() {
        let mut workspaces = new_workspaces("first payload");

        let result = workspaces.rename_workspace(WorkspaceId::new(99), "unknown".to_owned());

        assert_eq!(
            (result, workspaces.active().name()),
            (
                Err(WorkspaceError::WorkspaceNotFound(WorkspaceId::new(99))),
                "Default",
            )
        );
    }

    #[test]
    fn removing_the_final_workspace_should_leave_an_empty_collection_without_a_replacement() {
        let mut workspaces = new_workspaces("first payload");
        workspaces.next_workspace_id = u64::MAX;

        let removed = workspaces.remove_workspace(WorkspaceId::new(1));

        assert_eq!(
            (removed, workspaces.len(), workspaces.active_workspace_id()),
            (Ok("first payload"), 0, None)
        );
    }

    #[test]
    fn removing_the_active_workspace_should_activate_the_workspace_that_takes_its_place() {
        let mut workspaces = new_workspaces("first payload");
        let second = workspaces
            .create_local_workspace(validated("/second", 0), |_, _| "second payload")
            .unwrap();
        workspaces
            .create_local_workspace(validated("/third", 0), |_, _| "third payload")
            .unwrap();
        workspaces.activate_workspace(second).unwrap();

        workspaces.remove_workspace(second).unwrap();

        assert_eq!(workspaces.active_workspace_id(), Some(WorkspaceId::new(3)));
    }

    #[test]
    fn creating_a_workspace_in_an_empty_collection_should_activate_it_with_a_fresh_id() {
        let mut workspaces = new_workspaces("first payload");
        workspaces.remove_workspace(WorkspaceId::new(1)).unwrap();

        let created = workspaces
            .create_local_workspace(validated("/next", 0), |_, _| "next payload")
            .unwrap();

        assert_eq!(
            (created, workspaces.active_workspace_id()),
            (WorkspaceId::new(2), Some(WorkspaceId::new(2)))
        );
    }

    #[test]
    fn removing_an_unknown_workspace_should_leave_the_collection_unchanged() {
        let mut workspaces = new_workspaces("first payload");

        let removed = workspaces.remove_workspace(WorkspaceId::new(99));

        assert_eq!(
            (removed, workspaces.len()),
            (
                Err(WorkspaceError::WorkspaceNotFound(WorkspaceId::new(99))),
                1
            )
        );
    }

    #[test]
    fn removed_workspace_ids_should_not_be_reused() {
        let mut workspaces = new_workspaces("first payload");
        let second = workspaces
            .create_local_workspace(validated("/second", 0), |_, _| "second payload")
            .unwrap();
        workspaces.remove_workspace(second).unwrap();

        let third = workspaces
            .create_local_workspace(validated("/third", 0), |_, _| "third payload")
            .unwrap();

        assert_eq!(third, WorkspaceId::new(3));
    }

    #[test]
    fn removing_the_active_last_workspace_should_activate_the_previous_one() {
        let mut workspaces = new_workspaces("first payload");
        let second = workspaces
            .create_local_workspace(validated("/second", 0), |_, _| "second payload")
            .unwrap();

        workspaces.remove_workspace(second).unwrap();

        assert_eq!(workspaces.active_workspace_id(), Some(WorkspaceId::new(1)));
    }
}
