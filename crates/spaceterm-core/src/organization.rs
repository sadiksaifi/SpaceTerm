//! The retained shared hierarchy of a server. Personal arrangement and live terminal state are
//! absent.
use crate::{CoreError, storage::Directory};
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
    pub(crate) fn validate(&self) -> Result<(), CoreError> {
        if self.version != 1 {
            return Err(CoreError::UnsafeStorage);
        }
        self.shared.validate()
    }
}

/// The file that retains the organization, apart from the Machine ID in `identity.json`, so a
/// reset can replace one without the other.
pub(crate) const FILE: &str = "organization.json";
const FILE_NAME: &std::ffi::CStr = c"organization.json";

/// Whether `value` is an `organization.json` this build reads.
pub(crate) fn readable(value: &serde_json::Value) -> bool {
    Organization::deserialize(value).is_ok_and(|organization| organization.validate().is_ok())
}

/// The host hands this store to the runtime only while holding its instance lock.
///
/// [`Self::read`] confirms that what it returns is durable. A publication syncs its new file and
/// the directory after renaming it. Two syncs remain per publication.
pub struct OrganizationStore(pub(crate) Directory);
impl OrganizationStore {
    /// A Git child retains this lock across exec through [`WorktreeLock::into_inheritable`],
    /// fencing recovery after a server crash until the original write exits. Never infer
    /// process authority from a saved PID.
    pub fn worktree_lock(&self) -> Result<WorktreeLock, CoreError> {
        self.0.lock("worktree.lock", true)
    }
    pub fn read(&self) -> Result<Option<Organization>, CoreError> {
        let Some(bytes) = self.0.read_bytes(FILE)? else {
            return Ok(None);
        };
        // Valid JSON can retain a schema this build no longer understands.
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| CoreError::UnsafeStorage)?;
        let organization =
            Organization::deserialize(&value).map_err(|_| CoreError::IncompatibleState)?;
        organization.validate()?;
        Ok(Some(organization))
    }
    pub fn write(&self, organization: Option<&Organization>) -> Result<(), CoreError> {
        match organization {
            Some(value) => {
                value.validate()?;
                self.0.write_json(FILE_NAME, value)
            }
            None => self.0.remove_record(FILE_NAME),
        }
    }
}
