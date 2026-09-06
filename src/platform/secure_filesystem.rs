use std::any::Any;
use std::ffi::OsStr;
use std::path::Path;
use std::sync::Arc;

/// Content-free classifications for failures at the secure filesystem boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SecureFilesystemError {
    Missing,
    AlreadyExists,
    Unsafe,
    Unavailable,
}

/// Opaque authority for one identity-checked private directory.
#[derive(Clone)]
pub(crate) struct SecureDirectory(pub(super) Arc<dyn Any + Send + Sync>);

/// Opaque identity for one inspected directory entry.
#[derive(Clone)]
pub(crate) struct SecureEntryIdentity(pub(super) Arc<dyn Any + Send + Sync>);

/// Opaque prepared same-directory file replacement.
pub(crate) struct PreparedPrivateFile(pub(super) Box<dyn Any + Send>);

pub(crate) struct PrivateFileSnapshot {
    pub(crate) bytes: Vec<u8>,
    pub(crate) identity: SecureEntryIdentity,
}

impl SecureDirectory {
    #[cfg(test)]
    pub(crate) fn from_opaque(value: impl Any + Send + Sync) -> Self {
        Self(Arc::new(value))
    }

    #[cfg(test)]
    pub(crate) fn opaque_ref<T: Any>(&self) -> Option<&T> {
        self.0.downcast_ref()
    }
}

impl SecureEntryIdentity {
    #[cfg(test)]
    pub(crate) fn from_opaque(value: impl Any + Send + Sync) -> Self {
        Self(Arc::new(value))
    }

    #[cfg(test)]
    pub(crate) fn opaque_ref<T: Any>(&self) -> Option<&T> {
        self.0.downcast_ref()
    }
}

impl PreparedPrivateFile {
    #[cfg(test)]
    pub(crate) fn from_opaque(value: impl Any + Send) -> Self {
        Self(Box::new(value))
    }

    #[cfg(test)]
    pub(crate) fn into_opaque<T: Any>(self) -> Result<Box<T>, Self> {
        match self.0.downcast() {
            Ok(value) => Ok(value),
            Err(value) => Err(Self(value)),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SecureCommitOutcome {
    Committed,
    Conflict,
    CommittedButUnsynced,
}

/// Irreducible race-resistant filesystem mechanics used by portable storage policy.
///
/// Implementations retain native descriptors, identity fields, permission values, and native
/// errors inside opaque leases. Callers own naming, retry bounds, mutation ordering, and cleanup
/// policy.
pub(crate) trait SecureFilesystem: Send + Sync {
    fn open_private_directory(
        &self,
        path: &Path,
    ) -> Result<Option<SecureDirectory>, SecureFilesystemError>;

    fn ensure_private_directory(
        &self,
        path: &Path,
    ) -> Result<SecureDirectory, SecureFilesystemError>;

    fn create_private_child(
        &self,
        parent: &SecureDirectory,
        name: &OsStr,
    ) -> Result<SecureDirectory, SecureFilesystemError>;

    fn verify_directory(&self, directory: &SecureDirectory) -> Result<(), SecureFilesystemError>;

    fn remove_private_child(
        &self,
        parent: &SecureDirectory,
        name: &OsStr,
        child: &SecureDirectory,
    ) -> Result<(), SecureFilesystemError>;

    fn read_private_file(
        &self,
        directory: &SecureDirectory,
        name: &OsStr,
        maximum_bytes: usize,
    ) -> Result<Option<PrivateFileSnapshot>, SecureFilesystemError>;

    fn prepare_private_file(
        &self,
        directory: &SecureDirectory,
        target: &OsStr,
        bytes: &[u8],
        allocation_nonce: [u8; 16],
    ) -> Result<PreparedPrivateFile, SecureFilesystemError>;

    fn commit_private_file(
        &self,
        prepared: PreparedPrivateFile,
        expected: Option<&SecureEntryIdentity>,
    ) -> Result<SecureCommitOutcome, SecureFilesystemError>;

    fn register_socket(
        &self,
        directory: &SecureDirectory,
        name: &OsStr,
    ) -> Result<SecureEntryIdentity, SecureFilesystemError>;

    fn verify_socket(
        &self,
        directory: &SecureDirectory,
        name: &OsStr,
        identity: &SecureEntryIdentity,
    ) -> Result<(), SecureFilesystemError>;

    fn remove_socket(
        &self,
        directory: &SecureDirectory,
        name: &OsStr,
        identity: &SecureEntryIdentity,
    ) -> Result<(), SecureFilesystemError>;

    #[cfg(test)]
    fn create_private_artifact(
        &self,
        directory: &SecureDirectory,
        name: &OsStr,
    ) -> Result<(), SecureFilesystemError>;
}

impl std::fmt::Debug for SecureDirectory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SecureDirectory(..)")
    }
}

impl std::fmt::Debug for SecureEntryIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SecureEntryIdentity(..)")
    }
}
