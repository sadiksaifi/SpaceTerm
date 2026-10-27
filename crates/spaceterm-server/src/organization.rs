//! The first server-owned Workspace. Personal arrangement and live terminal state are absent.
use crate::{ServerError, storage::Directory};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// First-Workspace seed and retained shared hierarchy. The seed's sole Tab and Pane are its Roots.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Organization {
    pub version: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared: Option<Box<crate::hierarchy::State>>,
    pub workspace: [u8; 32],
    pub tab: [u8; 32],
    pub pane: [u8; 32],
    pub name: String,
    pub pinned_directory: Option<PathBuf>,
    pub starting_directory: PathBuf,
    pub confirmed_directory: Option<PathBuf>,
}
impl Organization {
    pub fn first(starting_directory: PathBuf) -> Result<Self, ServerError> {
        let value = Self {
            version: 1,
            shared: None,
            workspace: crate::protocol::random()?,
            tab: crate::protocol::random()?,
            pane: crate::protocol::random()?,
            name: "Default".into(),
            pinned_directory: None,
            starting_directory,
            confirmed_directory: None,
        };
        value.validate()?;
        Ok(value)
    }
    pub(crate) fn validate(&self) -> Result<(), ServerError> {
        let path_valid = |path: &std::path::Path| {
            use std::os::unix::ffi::OsStrExt;
            path.is_absolute()
                && path.as_os_str().len() <= 4096
                && !path.as_os_str().as_bytes().contains(&0)
        };
        if self.version != 1
            || self.name.len() > 256
            || self.name.is_empty()
            || self.name.chars().any(char::is_control)
            || self.workspace == [0; 32]
            || self.tab == [0; 32]
            || self.pane == [0; 32]
            || self.workspace == self.tab
            || self.workspace == self.pane
            || self.tab == self.pane
            || !path_valid(&self.starting_directory)
            || self
                .confirmed_directory
                .as_deref()
                .is_some_and(|p| !path_valid(p))
            || self
                .pinned_directory
                .as_deref()
                .is_some_and(|p| !path_valid(p))
        {
            return Err(ServerError::UnsafeStorage);
        }
        if let Some(state) = &self.shared {
            state.validate()?;
        }
        Ok(())
    }
    pub fn startup_directory(&self) -> &std::path::Path {
        self.confirmed_directory
            .as_deref()
            .unwrap_or(&self.starting_directory)
    }
}

/// The host hands this store to the runtime only while holding its instance lock.
///
/// [`Self::read`] confirms that what it returns is durable. A publication syncs its new file and
/// the directory after renaming it; that makes the whole record durable, so a publication does not
/// first confirm the record it replaces. Two syncs remain per publication.
pub struct OrganizationStore(pub(crate) Directory);
impl OrganizationStore {
    /// A Git child retains this lock across exec, fencing recovery after a server crash until
    /// the original write exits. Never infer process authority from a saved PID.
    pub fn worktree_lock(&self) -> Result<std::fs::File, ServerError> {
        self.0.lock("worktree.lock", true)
    }
    pub fn read(&self) -> Result<Option<Organization>, ServerError> {
        let _lock = self.0.lock("setup.lock", false)?;
        Ok(self.0.read()?.organization.clone())
    }
    pub fn write(&self, organization: Option<&Organization>) -> Result<(), ServerError> {
        if let Some(value) = organization {
            value.validate()?;
        }
        let _lock = self.0.lock("setup.lock", false)?;
        let mut record = self.0.read_unconfirmed()?;
        record.organization = organization.cloned();
        self.0.write(&record)
    }
}
