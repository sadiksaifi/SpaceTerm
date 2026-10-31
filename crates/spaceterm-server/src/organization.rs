//! The retained shared hierarchy of a server. Personal arrangement and live terminal state are
//! absent.
use crate::{ServerError, storage::Directory};
use serde::{Deserialize, Serialize};

/// The Worktree write lock. It ends when dropped unless a Git child must inherit it.
pub use crate::storage::Lock as WorktreeLock;

/// The retained shared hierarchy.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Organization {
    pub version: u16,
    pub shared: Box<crate::hierarchy::State>,
}
impl Organization {
    pub fn new(shared: crate::hierarchy::State) -> Self {
        Self {
            version: 1,
            shared: Box::new(shared),
        }
    }
    pub(crate) fn validate(&self) -> Result<(), ServerError> {
        if self.version != 1 {
            return Err(ServerError::UnsafeStorage);
        }
        self.shared.validate()
    }
}

/// The host hands this store to the runtime only while holding its instance lock.
///
/// [`Self::read`] confirms that what it returns is durable. A publication syncs its new file and
/// the directory after renaming it; that makes the whole record durable, so a publication does not
/// first confirm the record it replaces. Two syncs remain per publication.
pub struct OrganizationStore(pub(crate) Directory);
impl OrganizationStore {
    /// A Git child retains this lock across exec through [`WorktreeLock::into_inheritable`],
    /// fencing recovery after a server crash until the original write exits. Never infer
    /// process authority from a saved PID.
    pub fn worktree_lock(&self) -> Result<WorktreeLock, ServerError> {
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
